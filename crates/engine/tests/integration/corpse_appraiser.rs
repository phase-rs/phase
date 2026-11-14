//! Corpse Appraiser — ETB exile from a graveyard, then dig if a card was
//! put into exile this way.
//!
//! Oracle: "When this creature enters, exile up to one target creature card
//! from a graveyard. If a card is put into exile this way, look at the top
//! three cards of your library, then put one of those cards into your hand
//! and the rest into your graveyard."
//!
//! Also covers Mysterious Limousine, the other card whose rider the shared
//! "put into exile this way" grammar now conditions.

use engine::game::combat::AttackTarget;
use engine::game::scenario::GameRunner;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, Effect, QuantityExpr, TargetFilter, TargetRef, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{ExileLink, ExileLinkKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const CORPSE_APPRAISER: &str = "When this creature enters, exile up to one target creature card from a graveyard. If a card is put into exile this way, look at the top three cards of your library, then put one of those cards into your hand and the rest into your graveyard.";

fn floating_ubr() -> Vec<ManaUnit> {
    vec![
        ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
    ]
}

/// SHAPE: the leading-if "put into exile this way" peels so the look-at +
/// put-one-keep-rest chain can assemble as a `Dig` gated by
/// `ZoneChangedThisWay { destination: Exile }`.
#[test]
fn corpse_appraiser_etb_parses_exile_then_conditional_dig() {
    let parsed = parse_oracle_text(CORPSE_APPRAISER, "Corpse Appraiser", &[], &[], &[]);
    let etb = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ChangesZone)
        .expect("ETB trigger");
    let execute = etb.execute.as_ref().expect("execute");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                ..
            }
        ),
        "first instruction is exile from a graveyard, got {:?}",
        execute.effect
    );
    let dig = execute.sub_ability.as_ref().expect("dig sub_ability");
    let Effect::Dig {
        count,
        keep_count,
        destination,
        rest_destination,
        ..
    } = &*dig.effect
    else {
        panic!("expected Dig follow-up, got {:?}", dig.effect);
    };
    assert_eq!(*count, QuantityExpr::Fixed { value: 3 });
    assert_eq!(*keep_count, Some(1));
    assert_eq!(*destination, Some(Zone::Hand));
    assert_eq!(*rest_destination, Some(Zone::Graveyard));
    let Some(AbilityCondition::ZoneChangedThisWay {
        filter,
        destination: this_way_dest,
    }) = &dig.condition
    else {
        panic!(
            "Dig must be gated by ZoneChangedThisWay, got {:?}",
            dig.condition
        );
    };
    assert_eq!(*this_way_dest, Some(Zone::Exile));
    match filter {
        engine::types::ability::TargetFilter::Typed(typed) => {
            assert_eq!(typed.type_filters, vec![TypeFilter::Card]);
        }
        other => panic!("expected Typed Card this-way filter, got {other:?}"),
    }
}

/// CR 608.2c + CR 701.13a + CR 701.20e: exiling a graveyard creature card
/// makes the "put into exile this way" gate true, so the look-at-three
/// instruction surfaces a DigChoice (keep one, rest to graveyard).
#[test]
fn corpse_appraiser_etb_exile_surfaces_dig_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let appraiser = scenario
        .add_creature_to_hand_from_oracle(P0, "Corpse Appraiser", 3, 3, CORPSE_APPRAISER)
        .id();
    let gy_creature = scenario
        .add_creature_to_graveyard(P1, "Doomed Bear", 2, 2)
        .id();
    let mill_two = scenario.add_card_to_library_top(P0, "Mill Two");
    let mill_one = scenario.add_card_to_library_top(P0, "Mill One");
    let keep = scenario.add_card_to_library_top(P0, "Keep Me");
    scenario.with_mana_pool(P0, floating_ubr());

    let mut runner = scenario.build();
    let outcome = runner.cast(appraiser).target_object(gy_creature).resolve();

    assert_eq!(
        outcome.zone_of(gy_creature),
        Zone::Exile,
        "the targeted graveyard creature card must be exiled"
    );

    let WaitingFor::DigChoice {
        cards,
        keep_count,
        rest_destination,
        kept_destination,
        ..
    } = outcome.final_waiting_for()
    else {
        panic!(
            "expected DigChoice after a card is put into exile this way, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(*keep_count, 1);
    assert_eq!(*kept_destination, Some(Zone::Hand));
    assert_eq!(*rest_destination, Some(Zone::Graveyard));
    assert_eq!(cards.len(), 3, "look at the top three cards");
    assert!(
        cards.contains(&keep) && cards.contains(&mill_one) && cards.contains(&mill_two),
        "DigChoice must offer the three looked-at library cards, got {cards:?}"
    );

    runner
        .act(GameAction::SelectCards { cards: vec![keep] })
        .expect("DigChoice SelectCards must succeed");

    let state = runner.state();
    assert_eq!(
        state.objects[&keep].zone,
        Zone::Hand,
        "the chosen looked-at card goes to hand"
    );
    assert_eq!(
        state.objects[&mill_one].zone,
        Zone::Graveyard,
        "the rest of the looked-at cards go to the graveyard"
    );
    assert_eq!(
        state.objects[&mill_two].zone,
        Zone::Graveyard,
        "the rest of the looked-at cards go to the graveyard"
    );
}

/// CR 608.2c: with no creature card in any graveyard the optional "up to one"
/// target is left empty, nothing is put into exile, the parent event ledger is
/// empty, and the "put into exile this way" Dig must neither prompt nor move a
/// library card.
#[test]
fn corpse_appraiser_no_graveyard_creature_does_not_dig() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let appraiser = scenario
        .add_creature_to_hand_from_oracle(P0, "Corpse Appraiser", 3, 3, CORPSE_APPRAISER)
        .id();
    let library = [
        scenario.add_card_to_library_top(P0, "Library Three"),
        scenario.add_card_to_library_top(P0, "Library Two"),
        scenario.add_card_to_library_top(P0, "Library One"),
    ];
    scenario.with_mana_pool(P0, floating_ubr());

    let mut runner = scenario.build();
    let outcome = runner.cast(appraiser).resolve();

    assert!(
        !matches!(outcome.final_waiting_for(), WaitingFor::DigChoice { .. }),
        "no card was put into exile, so no DigChoice, got {:?}",
        outcome.final_waiting_for()
    );
    for id in library {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Library,
            "no library card may move"
        );
    }
    assert_eq!(runner.state().players[0].graveyard.len(), 0);
}

const MYSTERIOUS_LIMOUSINE: &str = "Whenever this Vehicle enters or attacks, exile up to one other target creature until this Vehicle leaves the battlefield. If a creature is put into exile this way, return each other card exiled with this Vehicle to the battlefield under its owner's control.\nCrew 2";

/// SHAPE: the same grammar conditions Mysterious Limousine's return-each-other
/// continuation on "a creature put into exile this way" (it was unconditioned
/// before the "put into exile" arm existed).
#[test]
fn mysterious_limousine_return_is_gated_by_creature_put_into_exile() {
    let parsed = parse_oracle_text(
        MYSTERIOUS_LIMOUSINE,
        "Mysterious Limousine",
        &[],
        &["Artifact".to_string(), "Vehicle".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::EntersOrAttacks)
        .expect("enters-or-attacks trigger");
    let execute = trigger.execute.as_ref().expect("execute");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        ),
        "first instruction exiles the target creature, got {:?}",
        execute.effect
    );
    let ret = execute.sub_ability.as_ref().expect("return sub_ability");
    assert!(
        matches!(
            ret.effect.as_ref(),
            Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: Zone::Battlefield,
                ..
            }
        ),
        "second instruction returns the other exiled cards, got {:?}",
        ret.effect
    );
    let Some(AbilityCondition::ZoneChangedThisWay {
        filter,
        destination,
    }) = &ret.condition
    else {
        panic!(
            "return must be gated by ZoneChangedThisWay, got {:?}",
            ret.condition
        );
    };
    assert_eq!(*destination, Some(Zone::Exile));
    match filter {
        TargetFilter::Typed(typed) => {
            assert_eq!(typed.type_filters, vec![TypeFilter::Creature]);
        }
        other => panic!("expected Typed Creature this-way filter, got {other:?}"),
    }
}

/// Stage Mysterious Limousine on P0's battlefield with a card already exiled
/// with it (SYNTHETIC: linked as a prior activation's exile would be,
/// CR 607.2a; the link is added after build because a Vehicle that changed
/// zones would lose it, CR 400.7), a crewer, and an opposing creature. Crew it,
/// attack, and answer the attack trigger's optional target with `choice`.
/// Returns (runner, prior exiled card, opposing creature).
fn limousine_attack(choose_opposing: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let limo = scenario
        .add_creature(P0, "Mysterious Limousine", 4, 4)
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .from_oracle_text(MYSTERIOUS_LIMOUSINE)
        .id();
    let crewer = scenario.add_creature(P0, "Crewer", 2, 2).id();
    let prior = scenario
        .add_creature_to_exile(P1, "Previously Exiled Bear", 2, 2)
        .id();
    let victim = scenario.add_creature(P1, "Doomed Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: prior,
        source_id: limo,
        kind: ExileLinkKind::TrackedBySource,
    });

    // CR 702.122a: crew 2 with the 2/2.
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: limo,
            creature_ids: vec![],
        })
        .expect("enter crew selection");
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: limo,
            creature_ids: vec![crewer],
        })
        .expect("pay the crew cost");
    runner.advance_until_stack_empty();
    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(limo, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("the Limousine attacks");

    for _ in 0..40 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let legal = &target_slots[selection.current_slot].legal_targets;
                assert!(legal.contains(&TargetRef::Object(victim)));
                GameAction::ChooseTarget {
                    target: choose_opposing.then_some(TargetRef::Object(victim)),
                }
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("the attack trigger resolves");
    }
    assert!(runner.state().stack.is_empty(), "reach: trigger resolved");
    (runner, prior, victim)
}

/// CR 608.2c + CR 607.2a: a creature really is put into exile, so the
/// continuation runs: the previously exiled card returns, the new victim stays.
#[test]
fn mysterious_limousine_exiling_a_creature_returns_the_other_exiled_card() {
    let (runner, prior, victim) = limousine_attack(true);
    let state = runner.state();
    assert_eq!(state.objects[&victim].zone, Zone::Exile, "victim is exiled");
    assert_eq!(
        state.objects[&prior].zone,
        Zone::Battlefield,
        "the other card exiled with the Vehicle returns"
    );
}

/// CR 608.2c: the optional target is declined, so no creature is put into
/// exile, the gate is false and the previously exiled card must stay exiled.
/// At the merge base this continuation was unconditioned and returned it
/// anyway.
#[test]
fn mysterious_limousine_declined_target_returns_nothing() {
    let (runner, prior, victim) = limousine_attack(false);
    let state = runner.state();
    assert_eq!(state.objects[&victim].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&prior].zone,
        Zone::Exile,
        "nothing was put into exile this way, so nothing returns"
    );
}

/// CR 702.122a: crew `vehicle` by tapping `crewer`.
fn crew(runner: &mut GameRunner, vehicle: ObjectId, crewer: ObjectId) {
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: vec![],
        })
        .expect("enter crew selection");
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: vehicle,
            creature_ids: vec![crewer],
        })
        .expect("pay the crew cost");
    runner.advance_until_stack_empty();
}

/// CR 400.7 + CR 608.2c + CR 702.122a: the "creature" put into exile this way
/// is a crewed Vehicle, which stops being a creature the moment it leaves the
/// battlefield. The noun is read as it was when moved, so the gate still holds
/// and the card Limousine exiled earlier (through its own enters trigger) comes
/// back while the Vehicle stays exiled. The second Vehicle is SYNTHETIC
/// ("Parked Vehicle", Crew 1, no other text); the nearest printed analogue is
/// any crewed Vehicle, e.g. a second Mysterious Limousine.
#[test]
fn mysterious_limousine_exiling_a_crewed_vehicle_returns_the_other_exiled_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let limo = scenario
        .add_artifact_to_hand_from_oracle(P0, "Mysterious Limousine", MYSTERIOUS_LIMOUSINE)
        .with_subtypes(vec!["Vehicle"])
        .id();
    let parked = scenario
        .add_creature(P0, "Parked Vehicle", 3, 3)
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .from_oracle_text("Crew 1")
        .id();
    let limo_crewer = scenario.add_creature(P0, "Limousine Crewer", 2, 2).id();
    let parked_crewer = scenario.add_creature(P0, "Parked Crewer", 1, 1).id();
    let first_victim = scenario.add_creature(P1, "First Bear", 2, 2).id();
    // Both players must be able to draw through the turn cycle.
    for n in 0..3 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {n}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {n}"));
    }
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    // Enters trigger: the first exile happens through gameplay and is linked.
    runner.cast(limo).target_object(first_victim).resolve();
    assert_eq!(runner.state().objects[&first_victim].zone, Zone::Exile);

    // Cycle to P0's next precombat main, attacking and blocking with nothing.
    let start = runner.state().turn_number;
    for _ in 0..600 {
        let st = runner.state();
        if st.turn_number >= start + 2 && st.active_player == P0 && st.phase == Phase::PreCombatMain
        {
            break;
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("the turn cycle advances");
    }
    assert_eq!(runner.state().active_player, P0, "reach: P0's next turn");
    assert_eq!(runner.state().phase, Phase::PreCombatMain);

    crew(&mut runner, limo, limo_crewer);
    crew(&mut runner, parked, parked_crewer);
    assert!(
        runner.state().objects[&parked]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature),
        "reach: the crewed Vehicle is a creature on the battlefield"
    );

    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(limo, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("the Limousine attacks");
    for _ in 0..40 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let legal = &target_slots[selection.current_slot].legal_targets;
                assert!(legal.contains(&TargetRef::Object(parked)));
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(parked)),
                }
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("the attack trigger resolves");
    }
    assert!(runner.state().stack.is_empty(), "reach: trigger resolved");

    let state = runner.state();
    assert_eq!(
        state.objects[&parked].zone,
        Zone::Exile,
        "the crewed Vehicle was put into exile"
    );
    assert_eq!(
        state.objects[&first_victim].zone,
        Zone::Battlefield,
        "a creature was put into exile this way, so the other exiled card returns"
    );
}

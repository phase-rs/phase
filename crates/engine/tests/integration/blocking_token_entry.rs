use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ChosenAttribute, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{TrustedGameStateEnvelope, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

const BRIMAZ: &str = "Vigilance\nWhenever Brimaz attacks, create a 1/1 white Cat Soldier creature token with vigilance that's attacking.\nWhenever Brimaz blocks a creature, create a 1/1 white Cat Soldier creature token with vigilance that's blocking that creature.";
const FLASH_FOLIAGE: &str = "Cast this spell only during combat after blockers are declared.\nCreate a 1/1 green Saproling creature token that's blocking target creature attacking you.\nDraw a card.";
const HIGH_GROUND: &str = "Each creature you control can block an additional creature each combat.";
const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";

fn pass_to(runner: &mut GameRunner, ready: impl Fn(&WaitingFor) -> bool) {
    for _ in 0..24 {
        if ready(&runner.state().waiting_for) {
            return;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner.act(GameAction::PassPriority).unwrap();
    }
    panic!("combat priority boundary was not reached");
}

fn declare(
    runner: &mut GameRunner,
    attacks: Vec<(ObjectId, AttackTarget)>,
    assignments: Vec<(ObjectId, ObjectId)>,
) -> Vec<GameEvent> {
    pass_to(runner, |waiting| {
        matches!(waiting, WaitingFor::DeclareAttackers { .. })
    });
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .unwrap();
    pass_to(runner, |waiting| {
        matches!(waiting, WaitingFor::DeclareBlockers { .. })
    });
    let mut events = runner
        .act(GameAction::DeclareBlockers { assignments })
        .unwrap()
        .events;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        events.extend(
            runner
                .act(GameAction::DeclareBlockers {
                    assignments: vec![],
                })
                .unwrap()
                .events,
        );
    }
    events
}

#[test]
fn brimaz_uses_each_declared_block_pair_and_preserves_vigilance() {
    for attacker_count in [1, 2] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::BeginCombat);
        let brimaz = scenario
            .add_creature_from_oracle(P1, "Brimaz, King of Oreskos", 3, 4, BRIMAZ)
            .id();
        scenario.add_enchantment_from_oracle(P1, "High Ground", HIGH_GROUND);
        let attackers: Vec<_> = (0..attacker_count)
            .map(|_| scenario.add_creature(P0, "Attacker", 2, 2).id())
            .collect();
        let bystander = scenario.add_creature(P0, "Bystander", 2, 2).id();
        let mut runner = scenario.build();
        let declarations = declare(
            &mut runner,
            attackers
                .iter()
                .map(|&id| (id, AttackTarget::Player(P1)))
                .collect(),
            attackers.iter().map(|&id| (brimaz, id)).collect(),
        );
        // CR 509.3a-b: the actual declaration emits the blocker-side event.
        let declared_pairs: Vec<_> = declarations
            .iter()
            .filter_map(|event| match event {
                GameEvent::BlockersDeclared { assignments } => Some(assignments.as_slice()),
                _ => None,
            })
            .collect();
        assert_eq!(declared_pairs.len(), 1);
        let expected: Vec<_> = attackers.iter().map(|&id| (brimaz, id)).collect();
        assert_eq!(declared_pairs[0], expected);
        if attacker_count == 2 {
            let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for else {
                panic!("CR 509.3b: Brimaz triggers once per declared block pair");
            };
            assert_eq!(triggers.len(), 2);
            runner
                .act(GameAction::OrderTriggers { order: vec![1, 0] })
                .unwrap();
        }
        assert_eq!(runner.state().stack.len(), attacker_count);
        runner.advance_until_stack_empty();
        let state = runner.state();
        assert!(state.stack.is_empty());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        let combat = state.combat.as_ref().unwrap();
        for attacker in &attackers {
            // CR 509.4: each token blocks the attacker from its own triggering pair.
            let blockers = &combat.blocker_assignments[attacker];
            assert_eq!(blockers.len(), 2);
            let cat = blockers.iter().find(|&&id| id != brimaz).unwrap();
            assert!(state.objects[cat].is_token);
            assert!(state.objects[cat].keywords.contains(&Keyword::Vigilance));
            assert_eq!(combat.blocker_to_attacker[cat].as_slice(), &[*attacker]);
        }
        assert!(!combat.blocker_assignments.contains_key(&bystander));
    }
}

#[test]
fn flash_foliage_cast_creates_a_blocker_and_draws_with_multiplication() {
    for token_count in [1, 2] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::BeginCombat);
        scenario.add_creature(P1, "Available blocker", 2, 2);
        let flash = scenario
            .add_spell_to_hand_from_oracle(P1, "Flash Foliage", true, FLASH_FOLIAGE)
            .id();
        let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
        scenario.add_creature(P0, "Bystander", 2, 2);
        scenario.with_library_top(P1, &["Drawn card"]);
        if token_count == 2 {
            scenario.add_enchantment_from_oracle(P1, "Parallel Lives", PARALLEL_LIVES);
        }
        let mut runner = scenario.build();
        declare(
            &mut runner,
            vec![(attacker, AttackTarget::Player(P1))],
            vec![],
        );
        pass_to(
            &mut runner,
            |waiting| matches!(waiting, WaitingFor::Priority { player } if *player == P1),
        );
        let outcome = runner.cast(flash).target_object(attacker).resolve();
        outcome.assert_hand_drawn(P1, 1);
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert!(outcome.state().stack.is_empty());
        let combat = outcome.state().combat.as_ref().unwrap();
        let blockers = &combat.blocker_assignments[&attacker];
        assert_eq!(blockers.len(), token_count);
        assert!(blockers
            .iter()
            .all(|id| outcome.state().objects[id].is_token));
        // CR 509.3a-d: one first-blocked occurrence and one per entering blocker.
        assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::AttackerBecameBlocked { attacker: id, .. } if *id == attacker)).count(), 1);
        let entries: Vec<_> = outcome
            .events()
            .iter()
            .enumerate()
            .filter_map(|(index, event)| match event {
                GameEvent::AttackerBecameBlockedByFilteredBlocker {
                    attacker: blocked,
                    blocker,
                } if *blocked == attacker => Some((index, *blocker)),
                _ => None,
            })
            .collect();
        assert_eq!(entries.len(), token_count);
        for blocker in blockers {
            let occurrences: Vec<_> = entries.iter().filter(|(_, id)| id == blocker).collect();
            assert_eq!(occurrences.len(), 1);
            let entry_event = outcome
                .events()
                .iter()
                .position(|event| {
                    matches!(
                        event,
                        GameEvent::ZoneChanged {
                            object_id,
                            to: engine::types::zones::Zone::Battlefield,
                            ..
                        } if object_id == blocker
                    )
                })
                .expect("the blocker enters the battlefield");
            assert!(occurrences[0].0 < entry_event);
        }
        assert!(!outcome
            .events()
            .iter()
            .any(|event| matches!(event, GameEvent::BlockersDeclared { .. })));
    }
}

const UNSUMMON: &str = "Return target creature to its owner's hand.";

#[test]
fn brimaz_still_creates_a_vigilant_cat_after_the_attacker_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    let brimaz = scenario
        .add_creature_from_oracle(P1, "Brimaz, King of Oreskos", 3, 4, BRIMAZ)
        .id();
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .id();
    let mut runner = scenario.build();
    declare(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![(brimaz, attacker)],
    );
    assert_eq!(runner.state().stack.len(), 1);
    pass_to(
        &mut runner,
        |waiting| matches!(waiting, WaitingFor::Priority { player } if *player == P0),
    );
    runner.cast(bounce).target_object(attacker).commit();
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&attacker].zone,
        engine::types::zones::Zone::Hand
    );
    assert_eq!(runner.state().stack.len(), 1);
    runner.advance_until_stack_empty();

    // CR 509.4a: the bound object is no longer attacking. The Cat still enters.
    let state = runner.state();
    let cats: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].is_token)
        .collect();
    assert_eq!(cats.len(), 1);
    assert!(state.objects[cats[0]]
        .keywords
        .contains(&Keyword::Vigilance));
    assert!(!state
        .combat
        .as_ref()
        .unwrap()
        .blocker_to_attacker
        .contains_key(cats[0]));
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

#[test]
fn flash_foliage_does_not_draw_or_create_if_its_sole_target_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    scenario.add_creature(P1, "Available blocker", 2, 2);
    let flash = scenario
        .add_spell_to_hand_from_oracle(P1, "Flash Foliage", true, FLASH_FOLIAGE)
        .id();
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .id();
    scenario.with_library_top(P1, &["Still on top"]);
    let mut runner = scenario.build();
    declare(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![],
    );
    pass_to(
        &mut runner,
        |waiting| matches!(waiting, WaitingFor::Priority { player } if *player == P1),
    );
    let mut committed = runner.cast(flash).target_object(attacker).commit();
    assert_eq!(committed.state().stack.len(), 1);
    committed.act(GameAction::PassPriority).unwrap();
    committed.cast(bounce).target_object(attacker).commit();
    // CR 608.2b: the entire spell fails if its only target becomes illegal.
    let outcome = committed.resolve();
    outcome.assert_zone(&[attacker], engine::types::zones::Zone::Hand);
    outcome.assert_hand_drawn(P1, 0);
    assert!(!outcome
        .state()
        .battlefield
        .iter()
        .any(|id| outcome.state().objects[id].is_token));
    assert!(outcome.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn flash_foliage_announces_only_creatures_attacking_its_caster_directly() {
    let p2 = engine::types::player::PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::BeginCombat);
    scenario.add_creature(P1, "Available blocker", 2, 2);
    scenario.add_creature(p2, "Other player blocker", 2, 2);
    let flash = scenario
        .add_spell_to_hand_from_oracle(P1, "Flash Foliage", true, FLASH_FOLIAGE)
        .id();
    let direct = scenario.add_creature(P0, "Direct attacker", 2, 2).id();
    let second_direct = scenario
        .add_creature(P0, "Second direct attacker", 2, 2)
        .id();
    let walker_attacker = scenario
        .add_creature(P0, "Planeswalker attacker", 2, 2)
        .id();
    let siege_attacker = scenario.add_creature(P0, "Battle attacker", 2, 2).id();
    let other_attacker = scenario
        .add_creature(P0, "Other player attacker", 2, 2)
        .id();
    let bystander = scenario.add_creature(P0, "Bystander", 2, 2).id();
    let walker = scenario
        .add_planeswalker_from_oracle(P1, "Jace Beleren", "Jace", 3, "")
        .id();
    let siege = scenario.add_creature(P0, "Invasion of Zendikar", 0, 0).id();
    let mut runner = scenario.build();
    let battle = runner.state_mut().objects.get_mut(&siege).unwrap();
    battle.card_types.core_types = vec![CoreType::Battle];
    battle.card_types.subtypes = vec!["Siege".to_string()];
    battle.base_card_types = battle.card_types.clone();
    battle.defense = Some(3);
    battle.base_defense = Some(3);
    battle.counters.insert(CounterType::Defense, 3);
    battle.chosen_attributes.push(ChosenAttribute::Player(P1));

    declare(
        &mut runner,
        vec![
            (direct, AttackTarget::Player(P1)),
            (second_direct, AttackTarget::Player(P1)),
            (walker_attacker, AttackTarget::Planeswalker(walker)),
            (siege_attacker, AttackTarget::Battle(siege)),
            (other_attacker, AttackTarget::Player(p2)),
        ],
        vec![],
    );
    pass_to(
        &mut runner,
        |waiting| matches!(waiting, WaitingFor::Priority { player } if *player == P1),
    );
    let card_id = runner.state().objects[&flash].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: flash,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .unwrap();
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "CR 601.2c: Flash Foliage must ask for a target, got {:?}",
            runner.state().waiting_for,
        );
    };
    assert_eq!(*player, P1);
    assert_eq!(target_slots.len(), 1);
    assert_eq!(target_slots[0].legal_targets.len(), 2);
    for included in [direct, second_direct] {
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(included)));
    }
    for excluded in [walker_attacker, siege_attacker, other_attacker, bystander] {
        assert!(!target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(excluded)));
    }
}

#[test]
fn flash_foliage_keeps_its_attacker_when_a_copy_entry_pauses_and_restores() {
    const REFLECTION: &str = "Choose target nonlegendary creature. The next time one or more creatures or planeswalkers enter this turn, they enter as copies of the chosen creature.\nForetell {U}";
    const CLONE: &str =
        "You may have this creature enter as a copy of any creature on the battlefield.";
    const MIMIC: &str = "As this creature enters, choose a creature type.\nThis creature is the chosen type in addition to its other types.\nEach other creature you control of the chosen type enters with an additional +1/+1 counter on it.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    scenario.with_library_top(P1, &["Drawn card"]);
    let flash = scenario
        .add_spell_to_hand_from_oracle(P1, "Flash Foliage", true, FLASH_FOLIAGE)
        .id();
    let reflect = scenario
        .add_spell_to_hand_from_oracle(P1, "Mystic Reflection", true, REFLECTION)
        .id();
    // CR 613.4c + CR 704.5f: Anthem keeps the printed 0/0 Clone alive after it declines to copy.
    scenario.add_enchantment_from_oracle(P1, "Glorious Anthem", "Creatures you control get +1/+1.");
    let clone = scenario
        .add_creature_from_oracle(P1, "Clone", 0, 0, CLONE)
        .id();
    let mimic = scenario
        .add_creature_from_oracle(P1, "Metallic Mimic", 2, 1, MIMIC)
        .as_artifact_creature()
        .with_subtypes(vec!["Shapeshifter"])
        .id();
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let mut runner = scenario.build();
    declare(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![],
    );
    pass_to(
        &mut runner,
        |waiting| matches!(waiting, WaitingFor::Priority { player } if *player == P1),
    );
    runner.cast(reflect).target_object(clone).resolve();
    pass_to(
        &mut runner,
        |waiting| matches!(waiting, WaitingFor::Priority { player } if *player == P1),
    );
    runner.cast(flash).target_object(attacker).resolve();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .unwrap();
    }
    let WaitingFor::CopyTargetChoice { source_id, .. } = runner.state().waiting_for else {
        panic!("the token must offer Clone's copy choice");
    };
    let copy = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(mimic)),
        })
        .unwrap();
    let WaitingFor::NamedChoice {
        player, options, ..
    } = &copy.waiting_for
    else {
        panic!("the copy owes Metallic Mimic's as-enters choice");
    };
    assert_eq!(*player, P1);
    assert!(options.iter().any(|option| option == "Goblin"));
    assert!(!runner
        .state()
        .combat
        .as_ref()
        .unwrap()
        .blocker_assignments
        .contains_key(&attacker));
    let saved =
        serde_json::to_string(&TrustedGameStateEnvelope::capture(runner.state().clone())).unwrap();
    let pin = engine::types::identifiers::ObjectIncarnationRef::of(
        attacker,
        runner.state().objects[&attacker].incarnation,
    );
    let pending = runner
        .state()
        .pending_token_battlefield_entry
        .as_ref()
        .expect("copy entry publication must be parked");
    assert_eq!(pending.object_id, source_id);
    assert_eq!(pending.block_attacker, Some(pin));
    let entry_event = |event: &GameEvent| {
        matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                to: engine::types::zones::Zone::Battlefield,
                ..
            } if *object_id == source_id
        )
    };
    assert!(!copy.events.iter().any(entry_event));
    let restored: TrustedGameStateEnvelope = serde_json::from_str(&saved).unwrap();
    let mut resumed = GameRunner::from_state(restored.into_game_state());
    assert_eq!(
        resumed.state().pending_token_battlefield_entry.as_ref(),
        Some(pending)
    );
    let result = resumed
        .act(GameAction::ChooseOption {
            choice: "Goblin".into(),
        })
        .unwrap();
    let entry_index = result.events.iter().position(entry_event).unwrap();
    let first_blocked: Vec<_> = result.events.iter().enumerate().filter_map(|(index, event)| {
        matches!(event, GameEvent::AttackerBecameBlocked { attacker: id, .. } if *id == attacker)
            .then_some(index)
    }).collect();
    let per_blocker: Vec<_> = result.events.iter().enumerate().filter_map(|(index, event)| {
        matches!(event, GameEvent::AttackerBecameBlockedByFilteredBlocker { attacker: id, blocker } if *id == attacker && *blocker == source_id)
            .then_some(index)
    }).collect();
    assert_eq!(first_blocked.len(), 1);
    assert_eq!(per_blocker.len(), 1);
    assert!(first_blocked[0] < entry_index);
    assert!(per_blocker[0] < entry_index);
    resumed.advance_until_stack_empty();
    let state = resumed.state();
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    let blockers = &state.combat.as_ref().unwrap().blocker_assignments[&attacker];
    assert_eq!(blockers, &[source_id]);
    assert!(state.objects[&blockers[0]].is_token);
    assert_eq!(state.objects[&blockers[0]].name, "Metallic Mimic");
}

#[test]
fn blocking_rider_wire_is_additive() {
    use engine::types::ability::{AttackDefenderMatch, Effect, FilterProp, PtValue};
    use engine::types::game_state::PendingTokenBattlefieldEntry;
    use serde_json::{json, to_value};

    let legacy_attacking = json!({"type": "Attacking", "defender": "You"});
    let mut attacking: FilterProp = serde_json::from_value(legacy_attacking.clone()).unwrap();
    assert_eq!(to_value(&attacking).unwrap(), legacy_attacking);
    let FilterProp::Attacking { scope, .. } = &mut attacking else {
        panic!("legacy attacking filter");
    };
    assert_eq!(*scope, AttackDefenderMatch::DefendingSide);
    *scope = AttackDefenderMatch::DirectPlayer;
    let direct_attacking = json!({"type": "Attacking", "defender": "You", "scope": "DirectPlayer"});
    assert_eq!(to_value(&attacking).unwrap(), direct_attacking);
    assert_eq!(
        serde_json::from_value::<FilterProp>(direct_attacking).unwrap(),
        attacking
    );

    let legacy_effect = json!({
            "type": "Token",
            "name": "Cat Soldier",
            "power": PtValue::Fixed(1),
            "toughness": PtValue::Fixed(1),
            "types": ["Creature", "Cat", "Soldier"],
            "colors": [],
            "keywords": [],
            "tapped": false,
            "count": {"type": "Fixed", "value": 1},
            "owner": {"type": "Controller"},
            "enters_attacking": false
    });
    let mut with_attacker: Effect = serde_json::from_value(legacy_effect.clone()).unwrap();
    assert_eq!(to_value(&with_attacker).unwrap(), legacy_effect);
    let Effect::Token { block_attacker, .. } = &mut with_attacker else {
        panic!("legacy token effect");
    };
    *block_attacker = Some(Box::new(engine::types::ability::TargetFilter::ParentTarget));
    let encoded = to_value(&with_attacker).unwrap();
    assert_eq!(
        serde_json::from_value::<Effect>(encoded).unwrap(),
        with_attacker
    );

    let legacy_pending = json!({"object_id": 7, "name": "Cat Soldier", "source_id": 3});
    let mut pending: PendingTokenBattlefieldEntry =
        serde_json::from_value(legacy_pending.clone()).unwrap();
    assert_eq!(to_value(&pending).unwrap(), legacy_pending);
    pending.block_attacker = Some(engine::types::identifiers::ObjectIncarnationRef::of(
        ObjectId(9),
        4,
    ));
    let encoded = to_value(&pending).unwrap();
    assert_eq!(
        serde_json::from_value::<PendingTokenBattlefieldEntry>(encoded).unwrap(),
        pending
    );
}

#[test]
fn flash_foliage_redirected_entry_never_blocks() {
    const HALLOWED: &str = "Until end of turn, if a creature would enter and it wasn't cast, exile it instead.\nDraw a card.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    scenario.add_creature(P1, "Available blocker", 2, 2);
    scenario.with_library_top(P1, &["First draw", "Second draw"]);
    let flash = scenario
        .add_spell_to_hand_from_oracle(P1, "Flash Foliage", true, FLASH_FOLIAGE)
        .id();
    let moonlight = scenario
        .add_spell_to_hand_from_oracle(P1, "Hallowed Moonlight", true, HALLOWED)
        .id();
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let mut runner = scenario.build();
    declare(
        &mut runner,
        vec![(attacker, AttackTarget::Player(P1))],
        vec![],
    );
    pass_to(
        &mut runner,
        |waiting| matches!(waiting, WaitingFor::Priority { player } if *player == P1),
    );
    runner.cast(moonlight).resolve().assert_hand_drawn(P1, 1);
    pass_to(
        &mut runner,
        |waiting| matches!(waiting, WaitingFor::Priority { player } if *player == P1),
    );
    let outcome = runner.cast(flash).target_object(attacker).resolve();
    outcome.assert_hand_drawn(P1, 1);
    assert!(outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::ZoneChanged {
            to: engine::types::zones::Zone::Exile,
            ..
        }
    )));
    assert!(!outcome.events().iter().any(|event| matches!(
        event,
        GameEvent::AttackerBecameBlocked { .. }
            | GameEvent::AttackerBecameBlockedByFilteredBlocker { .. }
    )));
    assert!(!outcome
        .state()
        .combat
        .as_ref()
        .unwrap()
        .blocker_assignments
        .contains_key(&attacker));
    assert!(outcome.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
}

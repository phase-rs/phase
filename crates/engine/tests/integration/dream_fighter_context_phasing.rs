//! Dream Fighter's full MTGJSON Oracle text binds its combat partner without targeting.
use engine::game::ability_utils::declared_targets_in_chain;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameState, PersistedGameState, WaitingFor};
use engine::types::identifiers::{ObjectIncarnationRef, LEGACY_INCARNATION};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DREAM_FIGHTER: &str = "Whenever this creature blocks or becomes blocked by a creature, this creature and that creature phase out. (While they're phased out, they're treated as though they don't exist. Each one phases in before its controller untaps during their next untap step.)";
const CLOUDSHIFT: &str = "Exile target creature you control, then return that card to the battlefield under your control.";
// Full Oracle text verified against the local MTGJSON AtomicCards export.
const MIRROR_MATCH: &str = "Cast this spell only during the declare blockers step.\nFor each creature attacking you or a planeswalker you control, create a token that's a copy of that creature and that's blocking that creature. Exile those tokens at end of combat.";
const DRELNOCH: &str = "Whenever this creature becomes blocked, you may draw two cards.";
const ETERNAL_OF_HARSH_TRUTHS: &str = "Afflict 2 (Whenever this creature becomes blocked, defending player loses 2 life.)\nWhenever this creature attacks and isn't blocked, draw a card.";
const SMUGGLERS_COPTER: &str = "Flying\nWhenever this Vehicle attacks or blocks, you may draw a card. If you do, discard a card.\nCrew 1 (Tap any number of creatures you control with total power 1 or more: This Vehicle becomes an artifact creature until end of turn.)";
const CUNNING_EVASION: &str =
    "Whenever a creature you control becomes blocked, you may return it to its owner's hand.";

fn priority_to_combat_prompt(runner: &mut GameRunner, blockers: bool) {
    for _ in 0..32 {
        match runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } if blockers => return,
            WaitingFor::DeclareAttackers { .. } if !blockers => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            ref other => panic!("unexpected combat prompt: {other:?}"),
        }
    }
    panic!("combat declaration prompt not reached");
}

#[test]
fn dream_fighter_combat_partners_phase_out_without_announced_targets() {
    for fighter_attacks in [false, true] {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let fighter = scenario
            .add_creature(if fighter_attacks { P0 } else { P1 }, "Dream Fighter", 1, 1)
            .from_oracle_text(DREAM_FIGHTER)
            .id();
        let partner = scenario
            .add_creature(
                if fighter_attacks { P1 } else { P0 },
                "Combat partner",
                2,
                2,
            )
            .id();
        let bystander = scenario.add_creature(P1, "Bystander", 2, 2).id();
        let (attacker, blocker) = if fighter_attacks {
            (fighter, partner)
        } else {
            (partner, fighter)
        };
        let mut runner = scenario.build();

        priority_to_combat_prompt(&mut runner, false);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(attacker, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .unwrap();
        priority_to_combat_prompt(&mut runner, true);
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![(blocker, attacker)],
            })
            .unwrap();

        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        let triggers: Vec<_> = runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == fighter)
            .collect();
        assert_eq!(
            triggers.len(),
            1,
            "actual combat must trigger Dream Fighter"
        );
        let ability = triggers[0].ability().expect("triggered ability");
        assert!(declared_targets_in_chain(ability).is_empty());
        assert!(runner.state().objects[&fighter].is_phased_in());
        assert!(runner.state().objects[&partner].is_phased_in());

        runner.advance_until_stack_empty();
        assert!(runner.state().stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        for id in [fighter, partner] {
            let object = &runner.state().objects[&id];
            assert_eq!(object.zone, Zone::Battlefield);
            assert!(
                object.is_phased_out(),
                "both combat partners must phase out: {id:?}"
            );
        }
        assert!(runner.state().objects[&bystander].is_phased_in());
    }
}

// CR 509.3d + CR 509.4: an entering blocker triggers the attacker's
// per-creature ability, but never the copy's own "blocks" ability.
#[test]
fn dream_fighter_phases_each_mirror_match_copy_without_an_entering_blocks_trigger() {
    for mirror_count in [1, 2] {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let fighter = scenario
            .add_creature(P0, "Dream Fighter", 1, 1)
            .from_oracle_text(DREAM_FIGHTER)
            .id();
        let bystander = scenario.add_creature(P1, "Bystander", 2, 2).id();
        let mirrors: Vec<_> = (0..mirror_count)
            .map(|_| {
                scenario
                    .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH)
                    .with_mana_cost(ManaCost::Cost {
                        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                        generic: 4,
                    })
                    .id()
            })
            .collect();
        let islands: Vec<_> = (0..6 * mirror_count)
            .map(|_| scenario.add_basic_land(P1, ManaColor::Blue))
            .collect();
        let mut runner = scenario.build();
        priority_to_combat_prompt(&mut runner, false);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(fighter, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .unwrap();
        priority_to_combat_prompt(&mut runner, true);
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .unwrap();

        let mut copies = Vec::new();
        let mut pending_trigger_counts = Vec::new();
        for (index, mirror) in mirrors.into_iter().enumerate() {
            if matches!(runner.state().waiting_for, WaitingFor::Priority { player, .. } if player != P1)
            {
                runner.act(GameAction::PassPriority).unwrap();
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P1, .. }
            ));
            for &island in &islands[index * 6..(index + 1) * 6] {
                runner
                    .act(GameAction::ActivateAbility {
                        source_id: island,
                        ability_index: 0,
                        payment_mode: CastPaymentMode::Auto,
                    })
                    .unwrap();
            }
            drop(runner.cast(mirror).commit());
            let mut events = Vec::new();
            for _ in 0..8 {
                if runner.state().objects[&mirror].zone == Zone::Graveyard {
                    break;
                }
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                events.extend(runner.act(GameAction::PassPriority).unwrap().events);
            }
            assert_eq!(runner.state().objects[&mirror].zone, Zone::Graveyard);
            let created: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::TokenCreated {
                        object_id,
                        source_id,
                        ..
                    } if *source_id == mirror => Some(*object_id),
                    _ => None,
                })
                .collect();
            assert_eq!(created.len(), 1, "the printed spell must create its copy");
            let copy = created[0];
            copies.push(copy);
            assert_eq!(runner.state().objects[&copy].name, "Dream Fighter");
            assert_eq!(runner.state().objects[&copy].controller, P1);
            assert!(runner.state().objects[&copy].is_token);
            assert!(runner.state().objects[&copy].is_phased_in());
            assert_eq!(
                runner
                    .state()
                    .combat
                    .as_ref()
                    .unwrap()
                    .blocker_assignments
                    .get(&fighter)
                    .unwrap()
                    .as_slice(),
                copies.as_slice(),
                "each copy must actually enter blocking its own original"
            );
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, GameEvent::BlockersDeclared { .. })),
                "entering blocking is not a blocker declaration"
            );
            pending_trigger_counts.push(runner.state().stack.len());
            for trigger in &runner.state().stack {
                assert_eq!(
                    trigger.source_id, fighter,
                    "the entering copy never blocked for trigger purposes"
                );
                assert!(declared_targets_in_chain(trigger.ability().unwrap()).is_empty());
            }
        }

        runner.advance_until_stack_empty();
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        for id in std::iter::once(fighter).chain(copies) {
            assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
            assert!(
                runner.state().objects[&id].is_phased_out(),
                "the original and each exact entering partner must phase out: {id:?}"
            );
        }
        assert!(runner.state().objects[&bystander].is_phased_in());
        assert_eq!(
            pending_trigger_counts,
            (1..=mirror_count).collect::<Vec<_>>(),
            "each entering copy must trigger only the original attacking fighter"
        );
    }
}

// CR 509.3c: the first entering blocker fires the bare form once; neither
// its paired filtered event nor another entering blocker can fire it again.
#[test]
fn mirror_match_fires_bare_becomes_blocked_only_for_the_first_copy() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature(P0, "Drelnoch", 3, 3)
        .from_oracle_text(DRELNOCH)
        .id();
    let bystander = scenario.add_creature(P1, "Bystander", 2, 2).id();
    let mirrors: Vec<_> = (0..2)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH)
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                    generic: 4,
                })
                .id()
        })
        .collect();
    let islands: Vec<_> = (0..12)
        .map(|_| scenario.add_basic_land(P1, ManaColor::Blue))
        .collect();
    scenario.with_library_top(
        P0,
        &["First reward", "Second reward", "Must stay", "Also stays"],
    );
    let mut runner = scenario.build();
    priority_to_combat_prompt(&mut runner, false);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .unwrap();
    priority_to_combat_prompt(&mut runner, true);
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![],
        })
        .unwrap();

    for (index, mirror) in mirrors.into_iter().enumerate() {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player, .. } if player != P1)
        {
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P1, .. }
        ));
        for &island in &islands[index * 6..(index + 1) * 6] {
            runner
                .act(GameAction::ActivateAbility {
                    source_id: island,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap();
        }
        let outcome = runner.cast(mirror).commit().accept_optional().resolve();
        outcome.assert_hand_drawn(P0, if index == 0 { 2 } else { 0 });
        assert_eq!(outcome.state().objects[&mirror].zone, Zone::Graveyard);
        assert_eq!(
            outcome
                .state()
                .combat
                .as_ref()
                .unwrap()
                .blocker_assignments
                .get(&attacker)
                .unwrap()
                .len(),
            index + 1,
            "the second cast must really add a second blocking copy"
        );
        assert!(outcome.state().stack.is_empty());
        assert!(outcome.state().resolution_stack.is_empty());
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
        assert!(outcome.state().objects[&bystander].is_phased_in());
    }
}

// CR 702.130a + CR 509.3c: Afflict consumes the first blocked transition,
// whether declared or caused by an entering blocker, never each later copy.
#[test]
fn mirror_match_fires_printed_afflict_only_on_the_first_blocked_transition() {
    for declared_blocker in [false, true] {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let attacker = scenario
            .add_creature(P0, "Eternal of Harsh Truths", 1, 3)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            })
            .with_subtypes(vec!["Zombie", "Cleric"])
            .from_oracle_text_with_keywords(&["Afflict"], ETERNAL_OF_HARSH_TRUTHS)
            .id();
        let blocker = scenario.add_creature(P1, "Declared blocker", 2, 2).id();
        let bystander = scenario.add_creature(P1, "Bystander", 2, 2).id();
        let mirrors: Vec<_> = (0..2)
            .map(|_| {
                scenario
                    .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH)
                    .with_mana_cost(ManaCost::Cost {
                        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                        generic: 4,
                    })
                    .id()
            })
            .collect();
        let islands: Vec<_> = (0..12)
            .map(|_| scenario.add_basic_land(P1, ManaColor::Blue))
            .collect();
        scenario.with_library_top(P0, &["Unblocked attack reward", "Must stay"]);
        let mut runner = scenario.build();
        priority_to_combat_prompt(&mut runner, false);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(attacker, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .unwrap();
        priority_to_combat_prompt(&mut runner, true);
        runner
            .act(GameAction::DeclareBlockers {
                assignments: if declared_blocker {
                    vec![(blocker, attacker)]
                } else {
                    vec![]
                },
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(runner.life(P1), if declared_blocker { 18 } else { 20 });
        assert_eq!(runner.life(P0), 20);
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));

        for (index, mirror) in mirrors.into_iter().enumerate() {
            if matches!(runner.state().waiting_for, WaitingFor::Priority { player, .. } if player != P1)
            {
                runner.act(GameAction::PassPriority).unwrap();
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P1, .. }
            ));
            for &island in &islands[index * 6..(index + 1) * 6] {
                runner
                    .act(GameAction::ActivateAbility {
                        source_id: island,
                        ability_index: 0,
                        payment_mode: CastPaymentMode::Auto,
                    })
                    .unwrap();
            }
            let outcome = runner.cast(mirror).resolve();
            assert_eq!(outcome.state().objects[&mirror].zone, Zone::Graveyard);
            let assignments =
                &outcome.state().combat.as_ref().unwrap().blocker_assignments[&attacker];
            assert_eq!(
                assignments.len(),
                index + 1 + usize::from(declared_blocker),
                "each paid Mirror Match must add a real entering blocker"
            );
            let copy = *assignments.last().unwrap();
            assert!(outcome.state().objects[&copy].is_token);
            assert_eq!(
                outcome.state().objects[&copy].name,
                "Eternal of Harsh Truths"
            );
            outcome.assert_life_delta(
                P1,
                if index == 0 && !declared_blocker {
                    -2
                } else {
                    0
                },
            );
            outcome.assert_life_delta(P0, 0);
            assert!(outcome.state().objects[&bystander].is_phased_in());
            assert_eq!(outcome.state().objects[&bystander].zone, Zone::Battlefield);
            assert!(outcome.state().stack.is_empty());
            assert!(outcome.state().resolution_stack.is_empty());
            assert!(matches!(
                outcome.final_waiting_for(),
                WaitingFor::Priority { .. }
            ));
        }
        assert_eq!(runner.life(P1), 18, "Afflict must happen exactly once");
    }
}

// CR 707.2 + CR 506.3a: crew's animation isn't copied. The resulting Vehicle
// enters, but cannot block, trigger Cunning Evasion, or stop combat damage.
#[test]
fn mirror_match_noncreature_vehicle_copy_stays_unblocked_and_expires_at_end_of_combat() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let copter = scenario
        .add_creature(P0, "Smuggler's Copter", 3, 3)
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        })
        .from_oracle_text_with_keywords(&["Flying", "Crew"], SMUGGLERS_COPTER)
        .id();
    let pilot = scenario
        .add_creature(P0, "Elite Vanguard", 2, 1)
        .with_subtypes(vec!["Human", "Soldier"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    // A legal flying blocker keeps the real declaration window open; P1
    // deliberately declines it before casting Mirror Match.
    let hawk = scenario
        .add_creature(P1, "Suntail Hawk", 1, 1)
        .with_subtypes(vec!["Bird"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .from_oracle_text_with_keywords(&["Flying"], "Flying")
        .id();
    let evasion = scenario
        .add_enchantment_from_oracle(P0, "Cunning Evasion", CUNNING_EVASION)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();
    let mirror = scenario
        .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR_MATCH)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 4,
        })
        .id();
    let islands: Vec<_> = (0..6)
        .map(|_| scenario.add_basic_land(P1, ManaColor::Blue))
        .collect();
    // Keep a second card in hand so the printed loot requires a public discard
    // selection rather than automatically discarding its sole drawn card.
    let kept_card = scenario.add_land_to_hand(P0, "Island").id();
    let drawn_card = scenario.add_land_to_library_top(P0, "Plains").id();
    scenario.add_land_to_hand(P1, "Island");
    scenario.add_land_to_library_top(P1, "Plains");
    let mut runner = scenario.build();
    assert!(
        !runner.state().objects[&copter]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "the printed Artifact Vehicle must begin unanimated"
    );

    // CR 702.122a: pay the actual crew cost through the public selection prompt.
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: copter,
            creature_ids: vec![],
        })
        .unwrap();
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::CrewVehicle {
            player: P0,
            vehicle_id,
            eligible_creatures,
            ..
        } if *vehicle_id == copter && eligible_creatures.contains(&pilot)
    ));
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: copter,
            creature_ids: vec![pilot],
        })
        .unwrap();
    assert!(runner.state().objects[&pilot].tapped, "crew must be paid");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&copter]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "resolving crew must animate the original Vehicle"
    );
    priority_to_combat_prompt(&mut runner, false);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(copter, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .unwrap();

    let mut attack_events = Vec::new();
    let mut accepted_attack_loot = false;
    let mut selected_attack_discard = false;
    for _ in 0..32 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { .. } => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice {
                player, source_id, ..
            } => {
                assert_eq!(player, P0);
                assert_eq!(source_id, copter);
                assert!(!accepted_attack_loot, "only one attack loot is offered");
                accepted_attack_loot = true;
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                ..
            } => {
                assert_eq!(player, P0);
                assert_eq!(count, 1);
                assert!(cards.contains(&drawn_card));
                selected_attack_discard = true;
                GameAction::SelectCards {
                    cards: vec![drawn_card],
                }
            }
            other => panic!("unexpected Copter attack prompt: {other:?}"),
        };
        attack_events.extend(runner.act(action).unwrap().events);
    }
    assert!(accepted_attack_loot && selected_attack_discard);
    assert!(attack_events.iter().any(|event| matches!(
        event,
        GameEvent::CardDrawn { player_id: P0, object_id, .. } if *object_id == drawn_card
    )));
    assert_eq!(runner.state().objects[&drawn_card].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&kept_card].zone, Zone::Hand);
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![],
        })
        .unwrap();
    if matches!(runner.state().waiting_for, WaitingFor::Priority { player, .. } if player != P1) {
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1, .. }
    ));
    for island in islands {
        runner
            .act(GameAction::ActivateAbility {
                source_id: island,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
    }
    drop(runner.cast(mirror).commit());
    let mut copy_events = Vec::new();
    let mut optional_sources = Vec::new();
    for _ in 0..32 {
        if runner.state().objects[&mirror].zone == Zone::Graveyard
            && runner.state().stack.is_empty()
            && runner.state().resolution_stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice { source_id, .. } => {
                // Accept even an erroneous Cunning Evasion trigger so the
                // pre-fix witness actually returns the original to hand.
                optional_sources.push(source_id);
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::DiscardChoice { count, cards, .. } => GameAction::SelectCards {
                cards: cards.into_iter().take(count).collect(),
            },
            other => panic!("unexpected Mirror Match prompt: {other:?}"),
        };
        copy_events.extend(runner.act(action).unwrap().events);
    }
    assert_eq!(runner.state().objects[&mirror].zone, Zone::Graveyard);
    let copies: Vec<_> = copy_events
        .iter()
        .filter_map(|event| match event {
            GameEvent::TokenCreated {
                object_id,
                source_id,
                ..
            } if *source_id == mirror => Some(*object_id),
            _ => None,
        })
        .collect();
    assert_eq!(copies.len(), 1, "the paid spell must create its token");
    let copy = copies[0];
    let token = &runner.state().objects[&copy];
    assert_eq!(token.zone, Zone::Battlefield);
    assert!(token.is_token);
    assert_eq!(token.name, "Smuggler's Copter");
    assert_eq!(token.controller, P1);
    assert!(token.card_types.core_types.contains(&CoreType::Artifact));
    assert!(token
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Vehicle"));
    assert!(
        !token.card_types.core_types.contains(&CoreType::Creature),
        "CR 707.2: the copy must not inherit the paid crew animation"
    );
    assert_eq!(
        runner.state().objects[&copter].zone,
        Zone::Battlefield,
        "a noncreature copy cannot cause Cunning Evasion to return the attacker"
    );
    assert!(
        !optional_sources.contains(&evasion),
        "Cunning Evasion must not trigger without a real blocker"
    );
    assert!(
        !optional_sources.contains(&copy),
        "the entering token must not trigger its copied blocks loot"
    );
    assert!(
        !copy_events.iter().any(|event| matches!(
            event,
            GameEvent::AttackerBecameBlockedByEffect { .. }
                | GameEvent::AttackerBecameBlockedByFilteredBlocker { .. }
                | GameEvent::BlockersDeclared { .. }
        )),
        "a noncreature entrant must emit neither attacker-side nor blocker-side block events"
    );
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());

    let mut combat_events = Vec::new();
    for _ in 0..32 {
        if runner.state().phase == Phase::PostCombatMain {
            break;
        }
        let phase = runner.state().phase;
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        combat_events.extend(
            runner
                .act(GameAction::PassPriority)
                .unwrap()
                .events
                .into_iter()
                .map(|event| (phase, event)),
        );
    }
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert_eq!(
        runner.life(P1),
        17,
        "the unblocked original deals its normal three damage"
    );
    assert!(combat_events.iter().any(|(_, event)| matches!(
        event,
        GameEvent::DamageDealt {
            source_id,
            target: TargetRef::Player(P1),
            amount: 3,
            is_combat: true,
            ..
        } if *source_id == copter
    )));
    let expired: Vec<_> = combat_events
        .iter()
        .filter_map(|(phase, event)| match event {
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to: Zone::Exile,
                ..
            } => Some((*phase, *object_id)),
            _ => None,
        })
        .collect();
    assert_eq!(
        expired,
        vec![(Phase::EndCombat, copy)],
        "Mirror Match must still exile exactly its nonblocking token at end of combat"
    );
    for id in [copter, pilot, evasion, hawk] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
    }
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}

// CR 400.7 + CR 608.2k: a named, untargeted combat partner is the original
// object, not a new incarnation returned by a response. Oracle strings verified
// against data/mtgjson/AtomicCards.json.
#[test]
fn dream_fighter_does_not_phase_out_a_blinked_combat_partner() {
    for fighter_attacks in [false, true] {
        for trusted_restore in [None, Some(false), Some(true)] {
            let mut scenario = GameScenario::new_n_player(2, 42);
            scenario.at_phase(Phase::PreCombatMain);
            let partner_controller = if fighter_attacks { P1 } else { P0 };
            let fighter = scenario
                .add_creature(if fighter_attacks { P0 } else { P1 }, "Dream Fighter", 1, 1)
                .from_oracle_text(DREAM_FIGHTER)
                .id();
            let partner = scenario
                .add_creature(partner_controller, "Combat partner", 2, 2)
                .id();
            let bystander = scenario.add_creature(P1, "Bystander", 2, 2).id();
            let plains = scenario.add_basic_land(partner_controller, ManaColor::White);
            let blink = scenario
                .add_spell_to_hand_from_oracle(partner_controller, "Cloudshift", true, CLOUDSHIFT)
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::White],
                    generic: 0,
                })
                .id();
            let (attacker, blocker) = if fighter_attacks {
                (fighter, partner)
            } else {
                (partner, fighter)
            };
            let mut runner = scenario.build();
            priority_to_combat_prompt(&mut runner, false);
            runner
                .act(GameAction::DeclareAttackers {
                    attacks: vec![(attacker, AttackTarget::Player(P1))],
                    bands: vec![],
                })
                .unwrap();
            priority_to_combat_prompt(&mut runner, true);
            runner
                .act(GameAction::DeclareBlockers {
                    assignments: vec![(blocker, attacker)],
                })
                .unwrap();

            let original_partner_incarnation = runner.state().objects[&partner].incarnation;
            assert_eq!(runner.state().stack.len(), 1);
            let trigger = runner.state().stack.back().unwrap();
            assert_eq!(
                trigger.source_id, fighter,
                "actual blocking must trigger Dream Fighter"
            );
            let ability = trigger.ability().expect("triggered ability");
            assert!(declared_targets_in_chain(ability).is_empty());
            assert!(ability.chosen_target_slots.slots().is_empty());

            if matches!(runner.state().waiting_for, WaitingFor::Priority { player, .. } if player != partner_controller)
            {
                runner.act(GameAction::PassPriority).unwrap();
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player, .. } if player == partner_controller
            ));
            runner
                .act(GameAction::ActivateAbility {
                    source_id: plains,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .unwrap();
            drop(runner.cast(blink).target_object(partner).commit());
            assert_eq!(runner.state().stack.len(), 2, "blink must be a response");
            assert_eq!(runner.state().stack.back().unwrap().source_id, blink);

            let mut response_events = Vec::new();
            for _ in 0..16 {
                if runner.state().objects[&blink].zone == Zone::Graveyard {
                    break;
                }
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ));
                response_events.extend(runner.act(GameAction::PassPriority).unwrap().events);
            }
            assert_eq!(runner.state().objects[&blink].zone, Zone::Graveyard);
            let partner_moves: Vec<_> = response_events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::ZoneChanged {
                        object_id,
                        from,
                        to,
                        ..
                    } if *object_id == partner => Some((*from, *to)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                partner_moves,
                vec![
                    (Some(Zone::Battlefield), Zone::Exile),
                    (Some(Zone::Exile), Zone::Battlefield),
                ],
                "the public response must positively exile and return the partner"
            );
            assert_eq!(runner.state().objects[&partner].zone, Zone::Battlefield);
            assert_ne!(
                runner.state().objects[&partner].incarnation,
                original_partner_incarnation
            );
            assert!(runner.state().objects[&fighter].is_phased_in());
            assert!(runner.state().objects[&partner].is_phased_in());
            assert_eq!(
                runner.state().stack.len(),
                1,
                "original trigger must still await resolution"
            );

            if let Some(trusted) = trusted_restore {
                let saved = if trusted {
                    PersistedGameState::capture(runner.state().clone())
                } else {
                    PersistedGameState::Raw(Box::new(runner.state().clone()))
                };
                let bytes = serde_json::to_vec(&saved).unwrap();
                let restored: GameState = serde_json::from_slice::<PersistedGameState>(&bytes)
                    .unwrap()
                    .into_game_state()
                    .expect("restore the pending original combat trigger");
                runner = GameRunner::from_state(restored);
            }
            let ability = runner.state().stack.back().unwrap().ability().unwrap();
            assert!(declared_targets_in_chain(ability).is_empty());
            runner.advance_until_stack_empty();
            assert!(runner.state().stack.is_empty());
            assert!(runner.state().resolution_stack.is_empty());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(runner.state().objects[&fighter].is_phased_out());
            assert_eq!(runner.state().objects[&fighter].zone, Zone::Battlefield);
            assert!(runner.state().objects[&partner].is_phased_in());
            assert!(runner.state().objects[&bystander].is_phased_in());
        }
    }
}

fn restore_combat_state(state: GameState, trusted: bool) -> Result<GameState, String> {
    let saved = if trusted {
        PersistedGameState::capture(state)
    } else {
        PersistedGameState::Raw(Box::new(state))
    };
    let bytes = serde_json::to_vec(&saved).map_err(|error| error.to_string())?;
    serde_json::from_slice::<PersistedGameState>(&bytes)
        .map_err(|error| error.to_string())?
        .into_game_state()
        .map_err(|error| error.to_string())
}

#[test]
fn dream_fighter_restore_refuses_an_unbound_live_combat_partner() {
    for partner_zone in [Zone::Battlefield, Zone::Exile] {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let fighter = scenario
            .add_creature(P1, "Dream Fighter", 1, 1)
            .from_oracle_text(DREAM_FIGHTER)
            .id();
        let partner = scenario.add_creature(P0, "Combat partner", 2, 2).id();
        let mut runner = scenario.build();
        priority_to_combat_prompt(&mut runner, false);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(partner, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .unwrap();
        priority_to_combat_prompt(&mut runner, true);
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![(fighter, partner)],
            })
            .unwrap();
        let mut captured = runner.state().clone();
        if partner_zone != Zone::Battlefield {
            // Infrastructure: an old save may already have lost this referent.
            engine::game::zones::move_to_zone(
                &mut captured,
                partner,
                partner_zone,
                &mut Vec::new(),
            );
        }
        let captured_pin = *captured
            .stack
            .back()
            .unwrap()
            .ability()
            .unwrap()
            .target_incarnations
            .iter()
            .find(|pin| pin.object_id == partner)
            .unwrap();
        let legacy_pin = ObjectIncarnationRef::of(partner, LEGACY_INCARNATION);
        for trusted in [false, true] {
            let restored = restore_combat_state(captured.clone(), trusted)
                .expect("captured current or stale pins remain valid authority");
            let mut control = GameRunner::from_state(restored);
            control.advance_until_stack_empty();
            assert!(control.state().objects[&fighter].is_phased_out());
            assert!(control.state().stack.is_empty());
            assert!(control.state().resolution_stack.is_empty());
            assert!(matches!(
                control.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            if partner_zone == Zone::Battlefield {
                assert!(control.state().objects[&partner].is_phased_out());
            } else {
                assert_eq!(control.state().objects[&partner].zone, Zone::Exile);
            }

            for (pins, exact_first) in [
                (Vec::new(), false),
                (vec![legacy_pin], false),
                (vec![legacy_pin, captured_pin], false),
                (vec![captured_pin, legacy_pin], true),
            ] {
                let mut legacy = captured.clone();
                legacy
                    .stack
                    .back_mut()
                    .unwrap()
                    .ability_mut()
                    .unwrap()
                    .set_target_incarnations_recursive(pins);
                let restored_legacy = restore_combat_state(legacy, trusted);
                if partner_zone == Zone::Battlefield && !exact_first {
                    assert!(
                        restored_legacy.is_err(),
                        "a live raw id cannot establish the original combat partner"
                    );
                } else {
                    let mut restored = GameRunner::from_state(
                        restored_legacy.expect("the referent has exact or departed authority"),
                    );
                    restored.advance_until_stack_empty();
                    assert!(restored.state().objects[&fighter].is_phased_out());
                    if partner_zone == Zone::Battlefield {
                        assert!(restored.state().objects[&partner].is_phased_out());
                    } else {
                        assert_eq!(restored.state().objects[&partner].zone, Zone::Exile);
                    }
                    assert!(restored.state().stack.is_empty());
                    assert!(restored.state().resolution_stack.is_empty());
                    assert!(matches!(
                        restored.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                }
            }
        }
    }
}

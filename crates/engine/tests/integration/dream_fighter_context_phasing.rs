//! Dream Fighter's full MTGJSON Oracle text binds its combat partner without targeting.
use engine::game::ability_utils::declared_targets_in_chain;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameState, PersistedGameState, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DREAM_FIGHTER: &str = "Whenever this creature blocks or becomes blocked by a creature, this creature and that creature phase out. (While they're phased out, they're treated as though they don't exist. Each one phases in before its controller untaps during their next untap step.)";
const CLOUDSHIFT: &str = "Exile target creature you control, then return that card to the battlefield under your control.";

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

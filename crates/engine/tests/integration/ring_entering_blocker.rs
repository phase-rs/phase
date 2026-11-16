//! CR 701.54c + CR 509.3d: entering blockers reach the Ring's per-creature trigger.
//! Mirror Match's own end-of-combat exile is countered with printed Stifle so a
//! battlefield-to-graveyard sacrifice cannot pass by observing its unrelated exile.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Full Oracle text verified against the canonical MTGJSON export.
const MYR: &str = "Indestructible (Damage and effects that say \"destroy\" don't destroy this creature. If its toughness is 0 or less, it still dies.)";
const MIRROR: &str = "Cast this spell only during the declare blockers step.\nFor each creature attacking you or a planeswalker you control, create a token that's a copy of that creature and that's blocking that creature. Exile those tokens at end of combat.";
const STIFLE: &str =
    "Counter target activated or triggered ability. (Mana abilities can't be targeted.)";

fn declaration_prompt(runner: &mut GameRunner, blockers: bool, protected: ObjectId) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } if !blockers => return,
            WaitingFor::DeclareBlockers { .. } if blockers => return,
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                ..
            } => {
                assert_eq!(player, P0);
                assert_eq!(count, 1, "the Ring's attack trigger discards one card");
                let discard = *cards
                    .iter()
                    .find(|id| **id != protected)
                    .expect("the Ring drew a card that can be discarded instead of Stifle");
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![discard],
                    })
                    .unwrap();
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected combat prompt: {other:?}"),
        }
    }
    panic!("required combat declaration was not reached");
}

#[test]
fn ring_level_three_sacrifices_the_exact_mirror_match_blocker_at_end_of_combat() {
    for level in [2, 3] {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let bearer = scenario
            .add_creature(P0, "Darksteel Myr", 0, 1)
            .from_oracle_text_with_keywords(&["Indestructible"], MYR)
            .id();
        // A legal, zero-power blocker ensures the declare-blockers prompt exists
        // even with the Ring's first-level greater-power evasion restriction.
        let bystander = scenario
            .add_creature(P1, "Darksteel Myr", 0, 1)
            .from_oracle_text_with_keywords(&["Indestructible"], MYR)
            .id();
        let mirror = scenario
            .add_spell_to_hand_from_oracle(P1, "Mirror Match", true, MIRROR)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 4,
            })
            .id();
        let stifle = scenario
            .add_spell_to_hand_from_oracle(P0, "Stifle", true, STIFLE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 0,
            })
            .id();
        let islands: Vec<_> = (0..6)
            .map(|_| scenario.add_basic_land(P1, ManaColor::Blue))
            .collect();
        let stifle_land = scenario.add_basic_land(P0, ManaColor::Blue);
        scenario.with_library_top(P0, &["Island", "Forest", "Plains"]);
        let mut runner = scenario.build();
        // Initial-position fixture: the player was already tempted this many
        // times before the public combat begins, as in the Ring unit fixtures.
        runner.state_mut().ring_level.insert(P0, level);
        runner.state_mut().ring_bearer.insert(P0, Some(bearer));
        runner.state_mut().layers_dirty.mark_full();

        declaration_prompt(&mut runner, false, stifle);
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(bearer, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .unwrap();
        declaration_prompt(&mut runner, true, stifle);
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .unwrap();
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player, .. } if player != P1)
        {
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
        let copy = {
            let outcome = runner.cast(mirror).commit().resolve();
            assert_eq!(outcome.state().objects[&mirror].zone, Zone::Graveyard);
            let copies: Vec<_> = outcome
                .events()
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
            assert_eq!(
                copies.len(),
                1,
                "the full printed Mirror Match created its blocker"
            );
            let copy = copies[0];
            assert_eq!(outcome.state().objects[&copy].controller, P1);
            assert_eq!(outcome.state().objects[&copy].zone, Zone::Battlefield);
            assert_eq!(
                outcome.state().combat.as_ref().unwrap().blocker_assignments[&bearer],
                vec![copy]
            );
            copy
        };

        // Both 0-power indestructible creatures survive combat independently of
        // the Ring. Reach the real delayed-trigger phase without constructing events.
        for _ in 0..64 {
            if runner.state().phase == Phase::EndCombat {
                break;
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(runner.state().phase, Phase::EndCombat);
        let mirror_expiry = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == mirror)
            .expect("Mirror Match's printed end-of-combat exile trigger is on the stack")
            .id;
        assert_eq!(
            runner.state().objects[&copy].zone,
            Zone::Battlefield,
            "the copied creature survived until the delayed-trigger window"
        );
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player, .. } if player != P0)
        {
            runner.act(GameAction::PassPriority).unwrap();
        }
        runner
            .act(GameAction::ActivateAbility {
                source_id: stifle_land,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        let outcome = runner
            .cast(stifle)
            .target_object(mirror_expiry)
            .commit()
            .resolve();
        assert_eq!(outcome.state().objects[&stifle].zone, Zone::Graveyard);
        let sacrificed = outcome.events().iter().any(|event| matches!(event,
            GameEvent::PermanentSacrificed { object_id, player_id } if *object_id == copy && *player_id == P1
        ));
        if level == 3 {
            assert!(
                sacrificed,
                "the level-three Ring trigger must make P1 sacrifice this exact entering blocker"
            );
            assert!(outcome.events().iter().any(|event| matches!(event,
                GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, .. } if *object_id == copy
            )));
            assert!(!outcome.state().battlefield.contains(&copy));
        } else {
            assert!(
                !sacrificed,
                "the paired lower-level Ring has no sacrifice ability"
            );
            assert_eq!(outcome.state().objects[&copy].zone, Zone::Battlefield);
        }
        assert_eq!(outcome.state().objects[&bearer].zone, Zone::Battlefield);
        assert_eq!(outcome.state().objects[&bystander].zone, Zone::Battlefield);
        assert!(outcome.state().stack.is_empty());
        assert!(outcome.state().resolution_stack.is_empty());
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
    }
}

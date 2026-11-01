//! Matt R15-2: a consumed one-shot prevention covering several simultaneous
//! combat portions publishes ONE aggregate `DamagePrevented` per (shield,
//! recipient) — one application is one trigger (CR 615.13), not one per
//! portion.
//!
//! Fixture (mirrored seats — P0 attacks P1, all on P0's turn): P0's 3/3
//! trampler X, blocked by P1's 1/1 Y, assigned 1 to Y plus 2 trample to P1.
//! P1's Jade Monolith redirects the 1 (X -> Y) to P1, so both portions reach
//! P1 from the same source X; P1's Awe Strike (cast targeting X after
//! attackers) prevents both — consumed at the first portion, ledger-eligible
//! at the second. The batch must publish exactly ONE (P1, 3) application, so
//! P1's Selfless Squire triggers ONCE: 3 + 1 (Hardened Scales) = 4 counters.
//!
//! Discriminating: pre-fix the applier emits per portion — (P1,1) + (P1,2) —
//! and the Squire triggers twice for (1+1) + (2+1) = 5 counters. Both the
//! `DamagePrevented` log shape and the counter total discriminate.
//!
//! Oracle texts are verbatim (Scryfall).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::impulsive_maneuvers_batch_shield::{
    assign_trample, damage_amounts, declare_attacks, reach_combat_damage,
};

/// Verbatim Selfless Squire (Scryfall; shared with
/// `selfless_squire_damage_prevented_trigger`).
const SELFLESS_SQUIRE_TEXT: &str = "Flash\nWhen this creature enters, prevent all damage that would be dealt to you this turn.\nWhenever damage that would be dealt to you is prevented, put that many +1/+1 counters on this creature.";
/// Verbatim Hardened Scales (shared with
/// `selfless_squire_damage_prevented_trigger`).
const HARDENED_SCALES_TEXT: &str = "If one or more +1/+1 counters would be put on a creature you control, put that many plus one +1/+1 counters on it instead.";
/// Verbatim Jade Monolith pattern (shared with
/// `damage_redirect_target_constraints`).
const JADE: &str = "{1}: The next time a source of your choice would deal damage to target creature this turn, that source deals that damage to you instead.";
/// Verbatim Awe Strike (Scryfall; shared with `awe_strike_prevention`).
const AWE_STRIKE: &str = "The next time target creature would deal damage this turn, prevent that damage. You gain life equal to the damage prevented this way.";

fn free_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: 0,
    }
}

fn add_mana(runner: &mut GameRunner, player: PlayerId, color: ManaType, count: usize) {
    for _ in 0..count {
        runner
            .state_mut()
            .add_mana_to_pool(player, ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
}

fn counters_on(runner: &GameRunner, obj: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&obj)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn prevented_pairs(events: &[GameEvent]) -> Vec<(TargetRef, u32)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamagePrevented { target, amount, .. } => Some((target.clone(), *amount)),
            _ => None,
        })
        .collect()
}

/// Pass priority until `player` holds it (P1 acts on P0's turn for the Jade
/// activation and the Awe cast alike).
fn priority(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player: current } if current == player)
        {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass to the acting player");
    }
    panic!("priority never reached {player:?}");
}

/// Announce Jade's printed activation declaring the creature target, through
/// the production pipeline (CR 115.1c + CR 602.2b).
fn announce_jade(runner: &mut GameRunner, jade: ObjectId, target: ObjectId) {
    priority(runner, P1);
    runner
        .act(GameAction::ActivateAbility {
            source_id: jade,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce Jade's printed activated ability");
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the declared creature target, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1);
    assert_eq!(target_slots.len(), 1);
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Object(target)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("submit a legal creature");
}

/// Resolve the activation through its on-resolution source choice
/// (CR 609.7a: the activator chooses on resolution).
fn choose_jade_source(runner: &mut GameRunner, source: ObjectId) {
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DamageSourceChoice { .. }
        ) {
            break;
        }
        assert!(
            !runner.state().stack.is_empty(),
            "the activation must reach its source choice"
        );
        runner
            .act(GameAction::PassPriority)
            .expect("resolve the activated ability");
    }
    let WaitingFor::DamageSourceChoice {
        player, options, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected a real source choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1);
    assert!(options.contains(&source));
    runner
        .act(GameAction::ChooseDamageSource { source })
        .expect("resume with the chosen source");
    assert!(runner.state().stack.is_empty());
}

#[test]
fn one_shot_prevention_covering_redirected_and_trample_portions_aggregates() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let x = scenario.add_creature(P0, "Trampler", 3, 3).trample().id();
    let y = scenario.add_creature(P1, "Blocker", 1, 1).id();
    let squire = scenario
        .add_creature_from_oracle(P1, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .id();
    scenario.add_enchantment_from_oracle(P1, "Hardened Scales", HARDENED_SCALES_TEXT);
    let jade = scenario
        .add_artifact_from_oracle(P1, "Jade Monolith", JADE)
        .id();
    let awe = scenario
        .add_spell_to_hand_from_oracle(P1, "Awe Strike", true, AWE_STRIKE)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();
    assert_eq!(
        counters_on(&runner, squire),
        0,
        "reach guard: the seeded Squire starts with no counters"
    );

    // Jade online before combat: X -> Y(1) will detour to P1.
    add_mana(&mut runner, P1, ManaType::Colorless, 1);
    announce_jade(&mut runner, jade, y);
    choose_jade_source(&mut runner, x);

    let mut log = declare_attacks(&mut runner, &[x]);

    // P1 casts Awe Strike targeting X in the post-attack window (an instant
    // on P0's turn — `cast` acts as whoever holds priority).
    let mut awe_cast = false;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { .. } => break,
            WaitingFor::Priority { player } if player == P1 && !awe_cast => {
                awe_cast = true;
                let outcome = runner.cast(awe).target_object(x).resolve();
                log.extend(outcome.events().iter().cloned());
            }
            WaitingFor::Priority { .. } => {
                log.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("advance the post-attack window")
                        .events,
                );
            }
            other => panic!("unexpected post-attack prompt: {other:?}"),
        }
    }
    assert!(awe_cast, "P1 must have cast Awe Strike targeting X");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ),
        "the window must end at blocker declaration"
    );

    log.extend(reach_combat_damage(&mut runner, &[(y, x)]));
    log.extend(assign_trample(&mut runner, x, &[(y, 1)], 2));

    // Portion 1 (X -> Y): redirect + Awe — answer redirect-first so both
    // portions reach P1 from X. Every later prompt must be a lone remainder.
    let mut replacement_prompts = 0;
    for _ in 0..64 {
        if matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ) {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice {
                candidate_count,
                candidates,
                ..
            } => {
                replacement_prompts += 1;
                if replacement_prompts == 1 {
                    assert_eq!(
                        candidate_count, 2,
                        "portion 1 must offer redirect + Awe, got {candidates:?}"
                    );
                    // Jade's redirect is hosted on the target creature (not the
                    // artifact), so the doors are told apart by description.
                    let redirect = candidates
                        .iter()
                        .position(|c| c.description == "One-shot damage replacement")
                        .unwrap_or_else(|| {
                            panic!("Jade's redirect must be offered, got {candidates:?}")
                        });
                    let awe_door = candidates
                        .iter()
                        .position(|c| c.description == "Prevent damage")
                        .unwrap_or_else(|| {
                            panic!("Awe must be offered alongside, got {candidates:?}")
                        });
                    assert_ne!(
                        redirect, awe_door,
                        "redirect and Awe must be distinct doors, got {candidates:?}"
                    );
                    log.extend(
                        runner
                            .act(GameAction::ChooseReplacement { index: redirect })
                            .expect("answer redirect-first")
                            .events,
                    );
                } else {
                    assert_eq!(
                        candidate_count, 1,
                        "only CR 616.1f lone remainders may follow, got {candidates:?}"
                    );
                    log.extend(
                        runner
                            .act(GameAction::ChooseReplacement { index: 0 })
                            .expect("answer the lone remainder")
                            .events,
                    );
                }
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                log.extend(
                    runner
                        .act(GameAction::OrderTriggers { order })
                        .expect("order triggers")
                        .events,
                );
            }
            WaitingFor::Priority { .. } => {
                log.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("drain to end of combat")
                        .events,
                );
            }
            other => panic!("unexpected combat prompt: {other:?}"),
        }
    }
    assert!(
        matches!(
            runner.state().phase,
            Phase::EndCombat | Phase::PostCombatMain
        ),
        "combat must complete"
    );

    assert_eq!(
        prevented_pairs(&log),
        vec![(TargetRef::Player(P1), 3)],
        "one application covering both P1 portions publishes ONE (P1,3) — \
         pre-fix the applier fragments it into (P1,1) + (P1,2)"
    );
    assert_eq!(
        counters_on(&runner, squire),
        4,
        "the Squire triggers ONCE for 3 damage, plus 1 from Scales — \
         pre-fix it triggers twice for (1+1) + (2+1) = 5"
    );
    assert_eq!(
        runner.state().objects[&y].damage_marked,
        0,
        "reach guard: the redirected 1 never touched Y"
    );
    assert_eq!(runner.state().objects[&y].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&x].damage_marked,
        1,
        "reach guard: Y's independent combat damage still occurred"
    );
    assert!(
        damage_amounts(&log, x, TargetRef::Player(P1)).is_empty(),
        "no trample may be dealt through"
    );
    assert_eq!(
        runner.life(P1),
        23,
        "P1 takes 0 and gains Awe's prevented total of 3"
    );
}

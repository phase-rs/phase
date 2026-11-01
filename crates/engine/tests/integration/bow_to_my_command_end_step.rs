//! Bow to My Command: the end-step payment enforces total power 8 (CR 118.12).
//!
//! Oracle: "(An ongoing scheme remains face up until it's abandoned.)
//! As you set this scheme in motion, choose an opponent.
//! Creatures the chosen player controls can't attack you or planeswalkers you control.
//! At the beginning of your opponents' end step, they may tap any number of untapped creatures they control with total power 8 or greater. If they do, abandon this scheme."
//!
//! CR 118.12 identifies the tap as "a cost, paid when the spell or ability
//! resolves", so the head lowers to an aggregate `PayCost(TapCreatures)` paid
//! by the triggering opponent — never to a bare `SetTapState`, which would
//! drop the total-power restriction and abandon unconditionally.
//!
//! Two rows, both in a 2-player Archenemy game with Bow face up (P1 chosen):
//! * below-eight: P1's creatures total 7 — no payment prompt is offered, the
//!   trigger drains silently, and Bow stays face up.
//! * eight: P1's creatures total exactly 8 — P1 is prompted, pays by tapping
//!   both, and Bow is abandoned (face down to the scheme-deck bottom).
//!
//! Two collective rows in a 4-player Archenemy game (P0 archenemy, P1/P2/P3
//! heroes sharing one end step per CR 805.4). The heroes pay Bow's tap cost
//! together (2017-06-13 ruling): each hero taps any number of their own
//! untapped creatures in seat order, and the COMBINED total must reach 8:
//! * split-eight: P1 taps a 4-power creature and P2 taps a 4-power creature
//!   (P3 passes with a 2-power creature on board) — combined 8 abandons Bow.
//! * split-seven: P1 holds 4 and P2 holds 3 — the team-wide 7 can't reach 8,
//!   so no may is offered at all, nothing taps, and Bow stays face up.
//! * partial-seven: P1 holds 4+4 and P2 holds 3 — the may IS offered (team
//!   total 11), but P1 contributes only one 4, so the combined 7 fails,
//!   NOTHING taps (selections tap simultaneously only once the combined
//!   total passes), and Bow stays face up.
//!
//! One departure row (CR 104.3a + CR 800.4f): P1 holds the collective prompt
//! and concedes — the session passes to the surviving heroes instead of
//! stranding, P2 + P3 tap for a combined 8, and Bow is abandoned.
//!
//! One stolen-creature row (CR 800.4a + CR 805.6): P1 answers with a 4-power
//! creature stolen from P0, then concedes while P2 is answering. The steal
//! ends on the way out and the creature returns to the archenemy — a living
//! non-payer — so it contributes no power and taps nothing: P2's own 4
//! leaves the combined total at 4, Bow stays face up, and the continuation
//! still drains.
//!
//! One signed-power row (CR 107.1b): P1 controls 4, 4 and −1 power. The
//! prompt is still offered (the can-pay gate sums positive-only power as an
//! upper bound: 8), but submitting all three is refused (signed total 7)
//! while submitting just the two 4s pays and abandons.
//!
//! Every row seats a 10-power P0 creature as a reach guard: with the wrong
//! payer (the archenemy) the aggregate prompt would offer it, and the
//! below-eight row would become payable.
//!
//! Revert-to-red: lower the head back to `SetTapState` — the below-eight row
//! abandons Bow unconditionally. Rebind the payer to the controller — the
//! eight row offers P0's creature instead of P1's. Drop the collective flag —
//! the split-eight row offers only P1's 4 power and never abandons. Drop the
//! departure reconciliation — the concession row strands the chain with the
//! survivors never prompted and Bow face up. Drop the paying-team check from
//! the reconciliation — the stolen-creature row abandons Bow off the
//! archenemy's returned 4 power and taps it. Drop the creature re-check —
//! the animator row taps the de-animated Sol Ring as part of the payment.

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, Effect, TapCreaturesRequirement, TapCreaturesSelectionMode, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::format::FormatConfig;
use engine::types::game_state::{CostResume, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

const BOW_TO_MY_COMMAND: &str = "(An ongoing scheme remains face up until it's abandoned.)\nAs you set this scheme in motion, choose an opponent.\nCreatures the chosen player controls can't attack you or planeswalkers you control.\nAt the beginning of your opponents' end step, they may tap any number of untapped creatures they control with total power 8 or greater. If they do, abandon this scheme.";

const ACT_OF_TREASON: &str = "Gain control of target creature until end of turn. Untap that creature. It gains haste until end of turn. (It can attack and {T} this turn.)";

const MARCH_OF_THE_MACHINES: &str = "Each noncreature artifact is an artifact creature with power and toughness each equal to its mana value. (Equipment that's a creature can't equip a creature.)";

const SOL_RING: &str = "{T}: Add {C}{C}.";

fn scheme_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Scheme".to_string()], &[]);
    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "{name}: document is gapped"
    );
    let mut face = CardFace {
        name: name.to_string(),
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Scheme);
    face.card_type.supertypes.push(Supertype::Ongoing);
    synthesize_archenemy(&mut face);
    face
}

/// Shared setup: Bow set in motion with P1 chosen, P1's creatures on the
/// battlefield at the given (power, toughness) pairs, a P0 10/10 reach
/// guard, P1 active in precombat main. Returns the runner, Bow's id, P1's
/// creatures, and P0's beater.
fn setup_end_step_game(p1_pts: &[(i32, i32)]) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    let mut p1_creatures = Vec::new();
    for (i, (power, toughness)) in p1_pts.iter().enumerate() {
        p1_creatures.push(
            scenario
                .add_creature(P1, &format!("Hero Bear {i}"), *power, *toughness)
                .id(),
        );
    }
    let beater = scenario.add_creature(P0, "Archenemy Beater", 10, 10).id();
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Filler {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Filler {i}"));
    }
    let mut runner = scenario.build();

    let face = scheme_face("Bow to My Command", BOW_TO_MY_COMMAND);
    create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    // CR 904.9: entering the archenemy's precombat main sets Bow in motion.
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }),
        "expected the as-choice prompt, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseOption {
            choice: "1".to_string(),
        })
        .expect("choosing P1 must be accepted");
    runner.advance_to_priority_window();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "as-choice must drain to priority, got {:?}",
        runner.state().waiting_for
    );

    let bow = *runner
        .state()
        .command_zone
        .iter()
        .find(|id| runner.state().objects[*id].name == "Bow to My Command")
        .expect("Bow must be face up in the command zone");
    assert!(
        !runner.state().objects[&bow].face_down,
        "Bow must be face up after being set in motion"
    );

    // Hand P1 the turn at precombat main; the tests drive to P1's end step.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, bow, p1_creatures, beater)
}

/// Drive P1's precombat main through an empty combat into P1's end step.
fn advance_to_p1_end_step(runner: &mut GameRunner) {
    runner.advance_to_combat();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("declare no attackers to cross combat");
    runner.advance_to_end_step();
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "must reach P1's end step, got {:?}",
        runner.state().phase
    );
    assert_eq!(
        runner.state().active_player,
        P1,
        "the end step must be P1's"
    );
}

/// CR 118.12: P1's creatures total 7 — below the floor. The trigger fires and
/// drains with NO payment prompt (unpayable, so the optional is suppressed),
/// nothing taps, and Bow stays face up.
#[test]
fn bow_end_step_below_eight_offers_no_payment_and_keeps_bow_face_up() {
    let (mut runner, bow, p1_creatures, beater) = setup_end_step_game(&[(4, 4), (3, 3)]);
    advance_to_p1_end_step(&mut runner);

    let mut saw_trigger = false;
    let mut reached_optional = false;
    let mut reached_prompt = false;
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                saw_trigger = true;
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .unwrap();
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                // Bug path: decline to drain, but the row still fails below.
                reached_optional = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .unwrap();
            }
            WaitingFor::PayCost { .. } => {
                // Bug path: record and stop draining; the row fails below.
                reached_prompt = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                if !runner.state().stack.is_empty() {
                    saw_trigger = true;
                }
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }

    assert!(
        saw_trigger,
        "Bow's end-step trigger must fire (else the row is vacuous)"
    );
    assert!(
        !reached_optional,
        "below eight total power, no optional payment may be offered"
    );
    assert!(
        !reached_prompt,
        "below eight total power, no tap-creatures prompt may appear"
    );
    assert!(
        runner.state().command_zone.contains(&bow),
        "Bow must stay face up in the command zone"
    );
    assert!(
        !runner.state().objects[&bow].face_down,
        "Bow must stay face up"
    );
    for id in &p1_creatures {
        assert!(
            !runner.state().objects[id].tapped,
            "nothing may tap when the floor is unreachable"
        );
    }
    assert!(
        !runner.state().objects[&beater].tapped,
        "the archenemy's creature must never be touched"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "clean drain back to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 118.12 + CR 601.2f: P1's creatures total exactly 8 — the floor. P1 is
/// prompted, taps both through the aggregate payment, and Bow is abandoned.
#[test]
fn bow_end_step_at_eight_taps_and_abandons() {
    let (mut runner, bow, p1_creatures, beater) = setup_end_step_game(&[(4, 4), (4, 4)]);
    advance_to_p1_end_step(&mut runner);

    let mut reached_optional = false;
    let mut reached_prompt = false;
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .unwrap();
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                reached_optional = true;
                assert_eq!(
                    player, P1,
                    "the triggering opponent announces the may, got {player:?}"
                );
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .unwrap();
            }
            WaitingFor::PayCost {
                player,
                kind,
                choices,
                resume,
                ..
            } => {
                reached_prompt = true;
                assert_eq!(player, P1, "the triggering opponent pays, got {player:?}");
                assert!(
                    matches!(kind, PayCostKind::TapCreatures { .. }),
                    "expected the aggregate tap-creatures payment, got {kind:?}"
                );
                assert!(
                    matches!(
                        kind,
                        PayCostKind::TapCreatures {
                            mode: TapCreaturesSelectionMode::Aggregate(_)
                        }
                    ),
                    "expected Aggregate selection semantics, got {kind:?}"
                );
                assert!(
                    matches!(resume, CostResume::Resolution),
                    "expected a resolution-time payment, got {resume:?}"
                );
                // Reach guard: exactly P1's untapped creatures are offered —
                // never the archenemy's 10-power beater.
                assert_eq!(
                    choices.len(),
                    p1_creatures.len(),
                    "only P1's creatures are eligible, got {choices:?}"
                );
                for id in &p1_creatures {
                    assert!(
                        choices.contains(id),
                        "P1's creature {id:?} must be offered, got {choices:?}"
                    );
                }
                assert!(
                    !choices.contains(&beater),
                    "the archenemy's creature must not be offered, got {choices:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: p1_creatures.clone(),
                    })
                    .unwrap();
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }

    assert!(reached_optional, "the optional payment must be offered");
    assert!(reached_prompt, "the tap-creatures prompt must appear");
    for id in &p1_creatures {
        assert!(
            runner.state().objects[id].tapped,
            "P1's paid creatures must be tapped"
        );
    }
    assert!(
        !runner.state().objects[&beater].tapped,
        "the archenemy's creature must stay untapped"
    );
    // CR 701.33b / CR 314.2: abandoned to the bottom of the scheme deck, face
    // down, out of the command zone.
    assert!(
        !runner.state().command_zone.contains(&bow),
        "Bow must leave the command zone once abandoned"
    );
    assert!(
        runner.state().objects[&bow].face_down,
        "abandoned Bow must be face down"
    );
    assert!(
        runner.state().scheme_deck.contains(&bow),
        "abandoned Bow must be back in the scheme deck"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "clean drain back to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// Shared 4-player setup: P0 archenemy with the 10/10 reach guard, P1/P2/P3
/// heroes with the given (power, toughness) pairs, Bow set in motion with P1
/// chosen, P1 active in precombat main. Returns the runner, Bow's id, each
/// hero's creatures in seat order, and P0's beater.
fn setup_end_step_game_4p(
    p1_pts: &[(i32, i32)],
    p2_pts: &[(i32, i32)],
    p3_pts: &[(i32, i32)],
) -> (GameRunner, ObjectId, Vec<Vec<ObjectId>>, ObjectId) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 4, 42);
    scenario.at_phase(Phase::Draw);
    let mut heroes = Vec::new();
    for (hero, pts) in [(P1, p1_pts), (P2, p2_pts), (P3, p3_pts)] {
        let mut creatures = Vec::new();
        for (i, (power, toughness)) in pts.iter().enumerate() {
            creatures.push(
                scenario
                    .add_creature(hero, &format!("Hero {hero:?} Bear {i}"), *power, *toughness)
                    .id(),
            );
        }
        heroes.push(creatures);
    }
    let beater = scenario.add_creature(P0, "Archenemy Beater", 10, 10).id();
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Filler {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Filler {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Filler {i}"));
        scenario.add_card_to_library_top(P3, &format!("P3 Filler {i}"));
    }
    let mut runner = scenario.build();

    let face = scheme_face("Bow to My Command", BOW_TO_MY_COMMAND);
    create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    // CR 904.9: entering the archenemy's precombat main sets Bow in motion.
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }),
        "expected the as-choice prompt, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseOption {
            choice: "1".to_string(),
        })
        .expect("choosing P1 must be accepted");
    runner.advance_to_priority_window();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "as-choice must drain to priority, got {:?}",
        runner.state().waiting_for
    );

    let bow = *runner
        .state()
        .command_zone
        .iter()
        .find(|id| runner.state().objects[*id].name == "Bow to My Command")
        .expect("Bow must be face up in the command zone");
    assert!(
        !runner.state().objects[&bow].face_down,
        "Bow must be face up after being set in motion"
    );

    // Hand P1 the shared heroes' turn at precombat main; the tests drive to
    // the heroes' shared end step.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, bow, heroes, beater)
}

/// CR 805.4 + 2017-06-13 ruling: two heroes each tap a 4-power creature and
/// a third hero passes — the COMBINED 8 abandons Bow. Each hero is prompted
/// separately for their own creatures only; the archenemy's beater is never
/// offered.
#[test]
fn bow_end_step_split_power_across_heroes_abandons() {
    let (mut runner, bow, heroes, beater) = setup_end_step_game_4p(&[(4, 4)], &[(4, 4)], &[(2, 2)]);
    advance_to_p1_end_step(&mut runner);
    let (p1_creatures, p2_creatures, p3_creatures) = (&heroes[0], &heroes[1], &heroes[2]);

    let mut reached_optional = false;
    let mut prompted = [false, false, false];
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .unwrap();
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                reached_optional = true;
                assert_eq!(
                    player, P1,
                    "the active hero announces the team may, got {player:?}"
                );
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .unwrap();
            }
            WaitingFor::PayCost {
                player,
                kind,
                choices,
                resume,
                ..
            } => {
                assert!(
                    matches!(
                        kind,
                        PayCostKind::TapCreatures {
                            mode: TapCreaturesSelectionMode::VariableX
                        }
                    ),
                    "each hero taps any subset of their own creatures, got {kind:?}"
                );
                assert!(
                    matches!(resume, CostResume::TeamTapPayment { .. }),
                    "expected the collective-payment session, got {resume:?}"
                );
                let (mine, slot, pass) = match player {
                    p if p == P1 => (p1_creatures, 0, false),
                    p if p == P2 => (p2_creatures, 1, false),
                    p if p == P3 => (p3_creatures, 2, true),
                    other => panic!("only heroes pay, got {other:?}"),
                };
                prompted[slot] = true;
                assert_eq!(
                    choices.len(),
                    mine.len(),
                    "only {player:?}'s own creatures are offered, got {choices:?}"
                );
                for id in mine.iter() {
                    assert!(
                        choices.contains(id),
                        "{player:?}'s creature {id:?} must be offered, got {choices:?}"
                    );
                }
                assert!(
                    !choices.contains(&beater),
                    "the archenemy's creature must not be offered, got {choices:?}"
                );
                let cards = if pass { vec![] } else { mine.clone() };
                runner.act(GameAction::SelectCards { cards }).unwrap();
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }

    assert!(reached_optional, "the team may must be offered");
    assert!(
        prompted == [true, true, true],
        "every hero with creatures must be prompted, got {prompted:?}"
    );
    for id in p1_creatures.iter().chain(p2_creatures.iter()) {
        assert!(
            runner.state().objects[id].tapped,
            "contributing heroes' creatures must be tapped"
        );
    }
    for id in p3_creatures {
        assert!(
            !runner.state().objects[id].tapped,
            "the passing hero's creature must stay untapped"
        );
    }
    assert!(
        !runner.state().objects[&beater].tapped,
        "the archenemy's creature must stay untapped"
    );
    assert!(
        !runner.state().command_zone.contains(&bow),
        "Bow must leave the command zone once abandoned"
    );
    assert!(
        runner.state().objects[&bow].face_down,
        "abandoned Bow must be face down"
    );
    assert!(
        runner.state().scheme_deck.contains(&bow),
        "abandoned Bow must be back in the scheme deck"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "clean drain back to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 104.3a + CR 800.4f + CR 805.6: P1 holds the collective payment prompt
/// and concedes. The session must NOT strand: the prompt passes to the next
/// surviving hero, P2 + P3 tap for a combined 8, and Bow is abandoned. P1's
/// creature left with them and taps nothing.
#[test]
fn bow_end_step_conceding_current_hero_passes_payment_to_survivors() {
    let (mut runner, bow, heroes, _) = setup_end_step_game_4p(&[(4, 4)], &[(4, 4)], &[(4, 4)]);
    advance_to_p1_end_step(&mut runner);
    let (p1_creatures, p2_creatures, p3_creatures) = (&heroes[0], &heroes[1], &heroes[2]);

    let mut conceded = false;
    let mut prompted_after: Vec<PlayerId> = Vec::new();
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .unwrap();
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .unwrap();
            }
            WaitingFor::PayCost { player, resume, .. } => {
                assert!(
                    matches!(resume, CostResume::TeamTapPayment { .. }),
                    "expected the collective-payment session, got {resume:?}"
                );
                if player == P1 && !conceded {
                    conceded = true;
                    runner
                        .act(GameAction::Concede { player_id: P1 })
                        .expect("concede must be accepted mid-prompt");
                    continue;
                }
                prompted_after.push(player);
                let mine = match player {
                    p if p == P2 => p2_creatures,
                    p if p == P3 => p3_creatures,
                    other => panic!("only surviving heroes pay, got {other:?}"),
                };
                runner
                    .act(GameAction::SelectCards {
                        cards: mine.clone(),
                    })
                    .unwrap();
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }

    assert!(conceded, "P1 must have held the prompt and conceded");
    assert_eq!(
        prompted_after,
        vec![P2, P3],
        "both surviving heroes must be prompted after the concession"
    );
    for id in p1_creatures {
        assert!(
            !runner.state().battlefield.contains(id),
            "the conceder's creature must have left the game"
        );
        assert!(
            !runner.state().objects[id].tapped,
            "the conceder's creature must tap nothing"
        );
    }
    for id in p2_creatures.iter().chain(p3_creatures.iter()) {
        assert!(
            runner.state().objects[id].tapped,
            "surviving contributors' creatures must be tapped"
        );
    }
    assert!(
        !runner.state().command_zone.contains(&bow),
        "Bow must leave the command zone once abandoned"
    );
    assert!(
        runner.state().objects[&bow].face_down,
        "abandoned Bow must be face down"
    );
    assert!(
        runner.state().scheme_deck.contains(&bow),
        "abandoned Bow must be back in the scheme deck"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the payment continuation must complete, not strand"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "clean drain back to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// Stolen-creature setup: P0 archenemy with the 10/10 beater plus a 4/4
/// mark, P1 with a zero-cost Act of Treason in hand and no creatures of
/// their own, P2 with one 4/4, P3 empty, Bow set in motion with P1 chosen,
/// P1 active in precombat main. Returns the runner, Bow's id, P2's
/// creatures, the mark, the beater, and the treason spell id.
fn setup_end_step_game_4p_with_steal() -> (
    GameRunner,
    ObjectId,
    Vec<ObjectId>,
    ObjectId,
    ObjectId,
    ObjectId,
) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 4, 42);
    scenario.at_phase(Phase::Draw);
    let p2_creatures = vec![scenario.add_creature(P2, "Hero P2 Bear 0", 4, 4).id()];
    let mark = scenario.add_creature(P0, "Archenemy Mark", 4, 4).id();
    let beater = scenario.add_creature(P0, "Archenemy Beater", 10, 10).id();
    let steal = scenario
        .add_spell_to_hand_from_oracle(P1, "Act of Treason", false, ACT_OF_TREASON)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Filler {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Filler {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Filler {i}"));
        scenario.add_card_to_library_top(P3, &format!("P3 Filler {i}"));
    }
    let mut runner = scenario.build();

    let face = scheme_face("Bow to My Command", BOW_TO_MY_COMMAND);
    create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    // CR 904.9: entering the archenemy's precombat main sets Bow in motion.
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }),
        "expected the as-choice prompt, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseOption {
            choice: "1".to_string(),
        })
        .expect("choosing P1 must be accepted");
    runner.advance_to_priority_window();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "as-choice must drain to priority, got {:?}",
        runner.state().waiting_for
    );

    let bow = *runner
        .state()
        .command_zone
        .iter()
        .find(|id| runner.state().objects[*id].name == "Bow to My Command")
        .expect("Bow must be face up in the command zone");
    assert!(
        !runner.state().objects[&bow].face_down,
        "Bow must be face up after being set in motion"
    );

    // Hand P1 the shared heroes' turn at precombat main; the test casts the
    // steal, then drives to the heroes' shared end step.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, bow, p2_creatures, mark, beater, steal)
}

/// CR 800.4a + CR 805.6 + CR 118.12: P1 steals P0's 4-power mark, answers
/// the collective payment with it, then concedes while P2 is answering.
/// The steal ends on the way out (CR 800.4a) and the mark returns to the
/// archenemy — alive, but not on the paying team — so the reconciliation
/// must prune it from `selected`: it contributes no power and taps nothing.
/// P2's own 4 leaves the combined total at 4, Bow stays face up, and the
/// continuation still drains instead of stranding.
#[test]
fn bow_end_step_conceded_stolen_creature_pays_nothing() {
    let (mut runner, bow, p2_creatures, mark, beater, steal) = setup_end_step_game_4p_with_steal();

    runner.cast(steal).target_object(mark).resolve();
    assert_eq!(
        runner.state().objects[&mark].controller,
        P1,
        "reach-guard: the steal must be in force before the end step"
    );

    advance_to_p1_end_step(&mut runner);

    let mut reached_optional = false;
    let mut p1_answered = false;
    let mut conceded = false;
    let mut prompted_after: Vec<PlayerId> = Vec::new();
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .unwrap();
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                reached_optional = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .unwrap();
            }
            WaitingFor::PayCost { player, resume, .. } => {
                let CostResume::TeamTapPayment { selected, .. } = resume else {
                    panic!("expected the collective-payment session, got {resume:?}");
                };
                if player == P1 {
                    assert!(!p1_answered, "P1 must answer exactly once before conceding");
                    assert!(
                        selected.is_empty(),
                        "P1 answers first with an empty session, got {selected:?}"
                    );
                    p1_answered = true;
                    runner
                        .act(GameAction::SelectCards { cards: vec![mark] })
                        .expect("P1's stolen mark must be selectable");
                    continue;
                }
                if player == P2 && !conceded {
                    assert_eq!(
                        selected,
                        vec![mark],
                        "P1's answer must have accumulated the mark before the concession"
                    );
                    conceded = true;
                    runner
                        .act(GameAction::Concede { player_id: P1 })
                        .expect("concede must be accepted mid-prompt");
                    continue;
                }
                assert_eq!(
                    player, P2,
                    "only the surviving contributor pays after the concession, got {player:?}"
                );
                prompted_after.push(player);
                runner
                    .act(GameAction::SelectCards {
                        cards: p2_creatures.clone(),
                    })
                    .unwrap();
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }

    assert!(
        reached_optional,
        "the team may must be offered (stolen 4 + P2's 4 reaches 8)"
    );
    assert!(p1_answered, "P1 must have answered with the mark");
    assert!(conceded, "P1 must have conceded mid-payment");
    assert_eq!(
        prompted_after,
        vec![P2],
        "P2 must keep answering after the concession"
    );
    assert!(
        runner.state().battlefield.contains(&mark),
        "the mark stays on the battlefield — its owner never left"
    );
    assert_eq!(
        runner.state().objects[&mark].controller,
        P0,
        "CR 800.4a: the steal ends and the mark returns to the archenemy"
    );
    assert!(
        !runner.state().objects[&mark].tapped,
        "the returned mark must tap nothing"
    );
    for id in &p2_creatures {
        assert!(
            !runner.state().objects[id].tapped,
            "shortfall: nothing taps when the combined 4 misses 8"
        );
    }
    assert!(
        !runner.state().objects[&beater].tapped,
        "the archenemy's beater must never be touched"
    );
    assert!(
        runner.state().command_zone.contains(&bow),
        "Bow must stay face up in the command zone"
    );
    assert!(
        !runner.state().objects[&bow].face_down,
        "Bow must stay face up on a combined 4"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the payment continuation must complete, not strand"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "clean drain back to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// March setup: P0 archenemy with the 10/10 beater, P1 with Sol Ring
/// (a 1/1 artifact creature under March) plus a 4/4 bear, P2 with a 4/4
/// bear, P3 with March of the Machines and no creatures, Bow set in motion
/// with P1 chosen, P1 active in precombat main. Returns the runner, Bow's
/// id, P1's bear, P2's bear, the Sol Ring, March, and the beater.
fn setup_end_step_game_4p_with_march() -> (
    GameRunner,
    ObjectId,
    ObjectId,
    ObjectId,
    ObjectId,
    ObjectId,
    ObjectId,
) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 4, 42);
    scenario.at_phase(Phase::Draw);
    let sol_ring = scenario
        .add_artifact_from_oracle(P1, "Sol Ring", SOL_RING)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    let p1_bear = scenario.add_creature(P1, "Hero P1 Bear", 4, 4).id();
    let p2_bear = scenario.add_creature(P2, "Hero P2 Bear", 4, 4).id();
    let march = scenario
        .add_enchantment_from_oracle(P3, "March of the Machines", MARCH_OF_THE_MACHINES)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    let beater = scenario.add_creature(P0, "Archenemy Beater", 10, 10).id();
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Filler {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Filler {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Filler {i}"));
        scenario.add_card_to_library_top(P3, &format!("P3 Filler {i}"));
    }
    let mut runner = scenario.build();

    let face = scheme_face("Bow to My Command", BOW_TO_MY_COMMAND);
    create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    // CR 904.9: entering the archenemy's precombat main sets Bow in motion.
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }),
        "expected the as-choice prompt, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseOption {
            choice: "1".to_string(),
        })
        .expect("choosing P1 must be accepted");
    runner.advance_to_priority_window();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "as-choice must drain to priority, got {:?}",
        runner.state().waiting_for
    );

    let bow = *runner
        .state()
        .command_zone
        .iter()
        .find(|id| runner.state().objects[*id].name == "Bow to My Command")
        .expect("Bow must be face up in the command zone");
    assert!(
        !runner.state().objects[&bow].face_down,
        "Bow must be face up after being set in motion"
    );

    // Hand P1 the shared heroes' turn at precombat main; the test drives to
    // the heroes' shared end step.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    (runner, bow, p1_bear, p2_bear, sol_ring, march, beater)
}

/// CR 800.4a + CR 805.6 + CR 118.12: P3's March of the Machines animates
/// P1's Sol Ring into a 1/1 artifact creature; P1 answers the collective
/// payment with it plus a 4-power bear, then P3 concedes while P2 is
/// answering. March leaves with its owner (CR 800.4a) and the layer pass
/// turns Sol Ring back into a noncreature artifact BEFORE the departure
/// reconciliation runs — so the reconciliation must prune it from
/// `selected`: it contributes no power and taps nothing. P1's and P2's own
/// 4s still reach the combined 8, Bow is abandoned, and the continuation
/// drains instead of stranding.
#[test]
fn bow_end_step_conceded_animator_artifact_pays_nothing() {
    let (mut runner, bow, p1_bear, p2_bear, sol_ring, march, beater) =
        setup_end_step_game_4p_with_march();

    advance_to_p1_end_step(&mut runner);

    // Reach guard: March animates Sol Ring into an artifact creature.
    assert!(
        runner.state().objects[&sol_ring]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "reach-guard: March must animate Sol Ring before the end step"
    );

    let mut reached_optional = false;
    let mut p1_answered = false;
    let mut conceded = false;
    let mut saw_pruned_resume = false;
    let mut prompted_after: Vec<PlayerId> = Vec::new();
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .unwrap();
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                reached_optional = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .unwrap();
            }
            WaitingFor::PayCost {
                player,
                choices,
                resume,
                ..
            } => {
                let CostResume::TeamTapPayment { selected, .. } = resume else {
                    panic!("expected the collective-payment session, got {resume:?}");
                };
                if player == P1 {
                    assert!(!p1_answered, "P1 must answer exactly once before conceding");
                    assert!(
                        selected.is_empty(),
                        "P1 answers first with an empty session, got {selected:?}"
                    );
                    assert!(
                        choices.contains(&sol_ring),
                        "the animated Sol Ring must be offered to P1, got {choices:?}"
                    );
                    p1_answered = true;
                    runner
                        .act(GameAction::SelectCards {
                            cards: vec![sol_ring, p1_bear],
                        })
                        .expect("P1's animated ring and bear must be selectable");
                    continue;
                }
                if player == P2 && !conceded {
                    assert_eq!(
                        selected,
                        vec![sol_ring, p1_bear],
                        "P1's answer must have accumulated the ring and bear before the concession"
                    );
                    conceded = true;
                    runner
                        .act(GameAction::Concede { player_id: P3 })
                        .expect("concede must be accepted mid-prompt");
                    // March leaves with its owner; Sol Ring is a noncreature again.
                    assert!(
                        !runner.state().battlefield.contains(&march),
                        "CR 800.4a: March must leave with conceding P3"
                    );
                    assert!(
                        !runner.state().objects[&sol_ring]
                            .card_types
                            .core_types
                            .contains(&CoreType::Creature),
                        "Sol Ring must be a noncreature artifact once March leaves"
                    );
                    continue;
                }
                assert_eq!(
                    player, P2,
                    "only the surviving contributor pays after the concession, got {player:?}"
                );
                assert_eq!(
                    selected,
                    vec![p1_bear],
                    "reconciliation must prune the de-animated ring from selected, got {selected:?}"
                );
                saw_pruned_resume = true;
                prompted_after.push(player);
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![p2_bear],
                    })
                    .unwrap();
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }

    assert!(
        reached_optional,
        "the team may must be offered (1 + 4 + 4 reaches 8)"
    );
    assert!(p1_answered, "P1 must have answered with the ring and bear");
    assert!(conceded, "P3 must have conceded mid-payment");
    assert!(
        saw_pruned_resume,
        "P2 must be re-prompted on the pruned session after the concession"
    );
    assert_eq!(
        prompted_after,
        vec![P2],
        "P2 must keep answering after the concession"
    );
    assert!(
        !runner.state().objects[&sol_ring].tapped,
        "the de-animated ring must tap nothing"
    );
    assert!(
        runner.state().objects[&p1_bear].tapped,
        "P1's bear paid and must be tapped"
    );
    assert!(
        runner.state().objects[&p2_bear].tapped,
        "P2's bear paid and must be tapped"
    );
    assert!(
        !runner.state().objects[&beater].tapped,
        "the archenemy's beater must never be touched"
    );
    // CR 701.33b / CR 314.2: abandoned to the bottom of the scheme deck, face
    // down, out of the command zone.
    assert!(
        !runner.state().command_zone.contains(&bow),
        "Bow must leave the command zone once abandoned"
    );
    assert!(
        runner.state().objects[&bow].face_down,
        "abandoned Bow must be face down"
    );
    assert!(
        runner.state().scheme_deck.contains(&bow),
        "abandoned Bow must be back in the scheme deck"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the payment continuation must complete, not strand"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "clean drain back to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 805.4 + 2017-06-13 ruling: P1 holds 4 and P2 holds 3 — the team-wide
/// 7 can't reach 8, so the trigger fires and drains with NO may prompt
/// (unpayable, so the optional is suppressed), nothing taps, and Bow stays
/// face up. Mirrors the 2-player below-eight row at team scale.
#[test]
fn bow_end_step_split_power_below_eight_taps_nothing_and_keeps_bow() {
    let (mut runner, bow, heroes, beater) = setup_end_step_game_4p(&[(4, 4)], &[(3, 3)], &[]);
    advance_to_p1_end_step(&mut runner);
    let (p1_creatures, p2_creatures) = (&heroes[0], &heroes[1]);
    assert!(heroes[2].is_empty());

    let mut saw_trigger = false;
    let mut reached_optional = false;
    let mut reached_prompt = false;
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                saw_trigger = true;
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .unwrap();
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                // Bug path: decline to drain, but the row still fails below.
                reached_optional = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .unwrap();
            }
            WaitingFor::PayCost { .. } => {
                // Bug path: record and stop draining; the row fails below.
                reached_prompt = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                if !runner.state().stack.is_empty() {
                    saw_trigger = true;
                }
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }

    assert!(
        saw_trigger,
        "Bow's end-step trigger must fire (else the row is vacuous)"
    );
    assert!(
        !reached_optional,
        "below eight team-wide power, no team may may be offered"
    );
    assert!(
        !reached_prompt,
        "below eight team-wide power, no tap-creatures prompt may appear"
    );
    assert!(
        runner.state().command_zone.contains(&bow),
        "Bow must stay face up in the command zone"
    );
    assert!(
        !runner.state().objects[&bow].face_down,
        "Bow must stay face up"
    );
    for id in p1_creatures.iter().chain(p2_creatures.iter()) {
        assert!(
            !runner.state().objects[id].tapped,
            "nothing may tap when the floor is unreachable"
        );
    }
    assert!(
        !runner.state().objects[&beater].tapped,
        "the archenemy's creature must never be touched"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "clean drain back to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 805.6: P1 holds 4+4 and P2 holds 3 — the may IS offered (team-wide 11
/// clears the gate), both heroes are prompted, but P1 contributes only one
/// 4, so the combined 7 fails. NOTHING taps — selections tap simultaneously
/// only once the combined total passes — and Bow stays face up. P3 controls
/// nothing and is skipped without a prompt.
#[test]
fn bow_end_step_partial_selection_below_eight_taps_nothing() {
    let (mut runner, bow, heroes, beater) =
        setup_end_step_game_4p(&[(4, 4), (4, 4)], &[(3, 3)], &[]);
    advance_to_p1_end_step(&mut runner);
    let (p1_creatures, p2_creatures) = (&heroes[0], &heroes[1]);
    assert!(heroes[2].is_empty());

    let mut reached_optional = false;
    let mut prompted_p1 = false;
    let mut prompted_p2 = false;
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .unwrap();
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                reached_optional = true;
                assert_eq!(
                    player, P1,
                    "the active hero announces the team may, got {player:?}"
                );
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .unwrap();
            }
            WaitingFor::PayCost {
                player,
                kind,
                choices,
                resume,
                ..
            } => {
                assert!(
                    matches!(
                        kind,
                        PayCostKind::TapCreatures {
                            mode: TapCreaturesSelectionMode::VariableX
                        }
                    ),
                    "each hero taps any subset of their own creatures, got {kind:?}"
                );
                assert!(
                    matches!(resume, CostResume::TeamTapPayment { .. }),
                    "expected the collective-payment session, got {resume:?}"
                );
                // P1 deliberately under-contributes: one 4 of two.
                let cards = match player {
                    p if p == P1 => {
                        prompted_p1 = true;
                        assert_eq!(
                            choices.len(),
                            p1_creatures.len(),
                            "both of P1's creatures are offered, got {choices:?}"
                        );
                        vec![p1_creatures[0]]
                    }
                    p if p == P2 => {
                        prompted_p2 = true;
                        assert_eq!(
                            choices.len(),
                            p2_creatures.len(),
                            "only P2's own creatures are offered, got {choices:?}"
                        );
                        p2_creatures.clone()
                    }
                    other => panic!("only P1/P2 hold creatures, got {other:?}"),
                };
                assert!(
                    !choices.contains(&beater),
                    "the archenemy's creature must not be offered, got {choices:?}"
                );
                runner.act(GameAction::SelectCards { cards }).unwrap();
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }

    assert!(reached_optional, "the team may must be offered");
    assert!(
        prompted_p1 && prompted_p2,
        "both contributing heroes must be prompted"
    );
    for id in p1_creatures.iter().chain(p2_creatures.iter()) {
        assert!(
            !runner.state().objects[id].tapped,
            "a failed combined payment must tap nothing"
        );
    }
    assert!(
        !runner.state().objects[&beater].tapped,
        "the archenemy's creature must stay untapped"
    );
    assert!(
        runner.state().command_zone.contains(&bow),
        "Bow must stay face up in the command zone"
    );
    assert!(
        !runner.state().objects[&bow].face_down,
        "Bow must stay face up"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "clean drain back to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 107.1b: P1 controls 4, 4 and −1 power. The prompt is still offered
/// (the can-pay gate sums positive-only power as its upper bound: 8), but
/// submitting all three is refused (their signed total power is 7) while
/// submitting just the two 4s pays and abandons.
#[test]
fn bow_end_step_negative_power_counts_against_the_total() {
    let (mut runner, bow, p1_creatures, beater) = setup_end_step_game(&[(4, 4), (4, 4), (-1, 1)]);
    advance_to_p1_end_step(&mut runner);
    let (pair, weakling) = (&p1_creatures[0..2], p1_creatures[2]);

    let mut reached_optional = false;
    let mut reached_prompt = false;
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .unwrap();
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                reached_optional = true;
                assert_eq!(player, P1, "P1 announces the may, got {player:?}");
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .unwrap();
            }
            WaitingFor::PayCost {
                player,
                kind,
                choices,
                ..
            } => {
                reached_prompt = true;
                assert_eq!(player, P1, "P1 pays, got {player:?}");
                assert!(
                    matches!(
                        kind,
                        PayCostKind::TapCreatures {
                            mode: TapCreaturesSelectionMode::Aggregate(_)
                        }
                    ),
                    "expected Aggregate selection semantics, got {kind:?}"
                );
                assert_eq!(
                    choices.len(),
                    3,
                    "every untapped creature is offered, got {choices:?}"
                );
                // CR 107.1b: 4 + 4 + (−1) is 7 — refused.
                let err = runner
                    .act(GameAction::SelectCards {
                        cards: p1_creatures.clone(),
                    })
                    .unwrap_err();
                assert!(
                    format!("{err:?}").contains('7'),
                    "the refusal must cite the signed total 7, got {err:?}"
                );
                // 4 + 4 is 8 — pays.
                runner
                    .act(GameAction::SelectCards {
                        cards: pair.to_vec(),
                    })
                    .unwrap();
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }

    assert!(reached_optional, "the optional payment must be offered");
    assert!(reached_prompt, "the tap-creatures prompt must appear");
    for id in pair {
        assert!(
            runner.state().objects[id].tapped,
            "the paid creatures must be tapped"
        );
    }
    assert!(
        !runner.state().objects[&weakling].tapped,
        "the refused creature must stay untapped"
    );
    assert!(
        !runner.state().objects[&beater].tapped,
        "the archenemy's creature must stay untapped"
    );
    assert!(
        !runner.state().command_zone.contains(&bow),
        "Bow must leave the command zone once abandoned"
    );
    assert!(
        runner.state().scheme_deck.contains(&bow),
        "abandoned Bow must be back in the scheme deck"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "clean drain back to priority, got {:?}",
        runner.state().waiting_for
    );
}

/// Parse pin: Bow's end-step "they may tap … with total power 8 or greater"
/// lowers to a COLLECTIVE aggregate cost (paid by the team, CR 805.6),
/// announced by the triggering player.
#[test]
fn bow_end_step_payment_parses_collective() {
    let parsed = parse_oracle_text(
        BOW_TO_MY_COMMAND,
        "Bow to My Command",
        &[],
        &["Scheme".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| {
            t.execute.as_ref().is_some_and(|d| {
                matches!(
                    &*d.effect,
                    Effect::PayCost {
                        cost: AbilityCost::TapCreatures { .. },
                        ..
                    }
                )
            })
        })
        .expect("Bow's end-step trigger must carry the tap-creatures cost");
    let def = trigger.execute.as_ref().expect("trigger has a body");
    match &*def.effect {
        Effect::PayCost {
            cost: AbilityCost::TapCreatures { requirement, .. },
            payer,
            ..
        } => {
            assert_eq!(
                *payer,
                TargetFilter::TriggeringPlayer,
                "the active hero announces the team may, got {payer:?}"
            );
            match requirement {
                TapCreaturesRequirement::Aggregate {
                    value, collective, ..
                } => {
                    assert_eq!(*value, 8, "the threshold must stay 8");
                    assert!(
                        *collective,
                        "the phase-head 'they may' tap cost must be collective"
                    );
                }
                other => panic!("expected the aggregate requirement, got {other:?}"),
            }
        }
        other => panic!("expected the PayCost head, got {other:?}"),
    }
}

//! CR 903.3d + CR 611.3a: statics gated on "as long as you control your commander" apply while their controller controls their own commander, and stop when they don't.

use engine::game::casting::can_activate_ability_now;
use engine::game::combat::AttackTarget;
use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::collections::HashSet;

const BALOTH: &str = "Trample\nLieutenant — As long as you control your commander, this creature gets +2/+2 and other creatures you control get +2/+2 and have trample.";
const CONVERGENCE: &str = "Dynastic Command Node — As long as you control your commander, activated abilities of cards in your graveyard cost {2} less to activate. This effect can't reduce the mana in that ability's activation cost to less than one mana.\nTranslocation Protocols — {3}, {T}: Mill three cards.";
const GOLLUM: &str = "Gollum can't block.\nWhen Gollum enters, exile up to one target card from an opponent's graveyard. Each opponent loses 2 life.\n{2}, Sacrifice an artifact or creature: Return this card from your graveyard to your hand. Activate only as a sorcery.";
const KRAKEN: &str = "Hexproof\nLieutenant — As long as you control your commander, this creature gets +2/+2 and has \"Whenever this creature becomes blocked, you may draw two cards.\"";
const DEMON: &str = "Flying\nLieutenant — As long as you control your commander, this creature gets +2/+2 and has \"Whenever this creature deals combat damage to a player, that player sacrifices a creature of their choice.\"";
const TYRANT: &str = "Flying, haste\nLieutenant — As long as you control your commander, this creature gets +2/+2 and has \"Whenever this creature attacks, it deals 7 damage to target creature defending player controls.\"";
const SKYHUNTER: &str = "Flying\nMelee (Whenever this creature attacks, it gets +1/+1 until end of turn for each opponent you attacked this combat.)\nLieutenant — As long as you control your commander, other creatures you control have melee.";

/// Recompute layers and read an object's effective (post-layer) power/toughness.
/// Mirrors `effective_pt` in `angelic_field_marshal_lieutenant_2885.rs`.
fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

/// True iff `id` currently has `keyword` after a fresh layer evaluation.
/// Mirrors `has_kw` in `angelic_field_marshal_lieutenant_2885.rs`.
fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

fn floating(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

/// CR 611.3a + CR 613.1f + CR 613.4c: Thunderfoot Baloth's other-creatures P/T
/// and trample clause must apply to OTHER creatures you control only, and only
/// while the gate is on; it must not leak onto — or fail to reach past — the
/// source itself.
#[test]
fn thunderfoot_baloth_lieutenant_splits_self_and_other_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let baloth = scenario
        .add_creature_from_oracle(P0, "Thunderfoot Baloth", 5, 5, BALOTH)
        .id();
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let opp = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let commander = scenario
        .add_creature(P0, "Test Commander", 1, 1)
        .commander()
        .id();
    let mut runner = scenario.build();

    // Gate ON.
    assert_eq!(
        effective_pt(&mut runner, baloth),
        (7, 7),
        "gate on: Baloth is base 5/5 + 2/2"
    );
    assert_eq!(
        effective_pt(&mut runner, bear),
        (4, 4),
        "gate on: other creature you control gets +2/+2"
    );
    assert!(
        has_kw(&mut runner, bear, &Keyword::Trample),
        "gate on: other creature you control gains trample"
    );
    assert_eq!(
        effective_pt(&mut runner, opp),
        (2, 2),
        "opponent's creature is untouched"
    );
    assert!(!has_kw(&mut runner, opp, &Keyword::Trample));

    // Gate OFF.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != commander);
        state.objects.remove(&commander);
    }
    assert_eq!(
        effective_pt(&mut runner, baloth),
        (5, 5),
        "gate off: Baloth returns to base 5/5"
    );
    assert_eq!(
        effective_pt(&mut runner, bear),
        (2, 2),
        "gate off: other creature loses the +2/+2"
    );
    assert!(
        !has_kw(&mut runner, bear, &Keyword::Trample),
        "gate off: other creature loses the granted trample"
    );
}

/// CR 601.2f + CR 602.2b + CR 118.7a: Convergence of Dominion's cost reduction
/// for graveyard cards floors at one mana, and does not touch its OWN
/// battlefield activated ability (which is not a card "in your graveyard").
#[test]
fn convergence_of_dominion_reduction_keeps_one_mana() {
    // Graveyard-source reduction: the gate floors the reduced cost at one mana.
    for (gate, mana, expect_can) in [
        (true, 0, false),
        (true, 1, true),
        (false, 1, false),
        (false, 2, true),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_artifact_from_oracle(P0, "Convergence of Dominion", CONVERGENCE);
        if gate {
            scenario
                .add_creature(P0, "Test Commander", 1, 1)
                .commander();
        }
        scenario.add_creature(P0, "Fodder", 1, 1);
        let gollum = scenario
            .add_creature_to_graveyard(P0, "Gollum the Abandoned", 2, 2)
            .from_oracle_text(GOLLUM)
            .id();
        if mana > 0 {
            scenario.with_mana_pool(P0, floating(mana));
        }
        let runner = scenario.build();
        // Reach guard: the ability is found by its activation zone, not by index
        // guesswork (CR 113.6m).
        let ability_idx = runner.state().objects[&gollum]
            .abilities
            .iter()
            .position(|a| a.activation_zone == Some(Zone::Graveyard))
            .expect("Gollum's graveyard-return ability must be found by its activation zone");
        assert_eq!(
            can_activate_ability_now(runner.state(), P0, gollum, ability_idx),
            expect_can,
            "gate={gate} mana={mana}"
        );
    }

    // Hostile scope row: Convergence's OWN {3}, {T} ability is never reduced,
    // gate on or off, because it is not a card in the graveyard.
    for (gate, mana, expect_can) in [
        (true, 2, false),
        (true, 3, true),
        (false, 2, false),
        (false, 3, true),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let convergence = scenario
            .add_artifact_from_oracle(P0, "Convergence of Dominion", CONVERGENCE)
            .id();
        if gate {
            scenario
                .add_creature(P0, "Test Commander", 1, 1)
                .commander();
        }
        if mana > 0 {
            scenario.with_mana_pool(P0, floating(mana));
        }
        let runner = scenario.build();
        assert_eq!(
            can_activate_ability_now(runner.state(), P0, convergence, 0),
            expect_can,
            "hostile scope: gate={gate} mana={mana}"
        );
    }
}

struct KrakenDrive {
    prompts: usize,
    hand_len: usize,
    blocked: bool,
    effective_pt: (i32, i32),
}

fn drive_kraken(gate: bool, num_blockers: usize, accept: bool) -> KrakenDrive {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kraken = scenario
        .add_creature_from_oracle(P0, "Stormsurge Kraken", 5, 5, KRAKEN)
        .id();
    if gate {
        scenario
            .add_creature(P0, "Test Commander", 1, 1)
            .commander();
    }
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C", "Card D"]);
    let blockers: Vec<ObjectId> = (0..num_blockers)
        .map(|i| scenario.add_creature(P1, &format!("Wall {i}"), 0, 8).id())
        .collect();
    let mut runner = scenario.build();

    let mut declared = false;
    let mut blocked = false;
    let mut prompts = 0;
    for _ in 0..400 {
        let wf = runner.state().waiting_for.clone();
        let res = match wf {
            WaitingFor::DeclareAttackers { player, .. } if player == P0 && !declared => {
                declared = true;
                runner.act(GameAction::DeclareAttackers {
                    attacks: vec![(kraken, AttackTarget::Player(P1))],
                    bands: vec![],
                })
            }
            WaitingFor::DeclareBlockers { .. } if !blocked => {
                blocked = true;
                let assignments = blockers.iter().map(|&b| (b, kraken)).collect();
                runner.act(GameAction::DeclareBlockers { assignments })
            }
            WaitingFor::OptionalEffectChoice { source_id, .. } if source_id == kraken => {
                prompts += 1;
                runner.act(GameAction::DecideOptionalEffect { accept })
            }
            WaitingFor::AssignCombatDamage { .. } => break,
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            _ => break,
        };
        if res.is_err() {
            break;
        }
    }

    let hand_len = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .hand
        .len();
    let pt = effective_pt(&mut runner, kraken);

    KrakenDrive {
        prompts,
        hand_len,
        blocked,
        effective_pt: pt,
    }
}

/// CR 509.3c + CR 603.5: Stormsurge Kraken's granted "becomes blocked" trigger
/// fires exactly once per combat regardless of blocker count, is optional, and
/// only exists while the gate is on.
#[test]
fn stormsurge_kraken_becomes_blocked_draws_two_while_lieutenant() {
    let accept = drive_kraken(true, 1, true);
    assert!(accept.blocked, "the Kraken must have been blocked");
    assert_eq!(
        accept.prompts, 1,
        "exactly one OptionalEffectChoice for one blocker"
    );
    assert_eq!(accept.hand_len, 2, "accepting draws two cards");
    assert_eq!(accept.effective_pt, (7, 7), "gate on: base 5/5 + 2/2");

    let decline = drive_kraken(true, 1, false);
    assert!(decline.blocked);
    assert_eq!(decline.prompts, 1);
    assert_eq!(decline.hand_len, 0, "declining draws nothing");

    // Two blockers: still exactly one prompt (CR 509.3c — the trigger fires
    // once per combat, not once per blocker); the drive loop stops at
    // WaitingFor::AssignCombatDamage.
    let two_blockers = drive_kraken(true, 2, true);
    assert!(two_blockers.blocked);
    assert_eq!(two_blockers.prompts, 1);
    assert_eq!(two_blockers.hand_len, 2);

    // Gate off: reach guard — the block still happens — but no granted
    // trigger fires.
    let gate_off = drive_kraken(false, 1, true);
    assert!(
        gate_off.blocked,
        "reach guard: the block must still happen with the gate off"
    );
    assert_eq!(
        gate_off.prompts, 0,
        "no granted trigger without the commander gate"
    );
    assert_eq!(gate_off.hand_len, 0);
    assert_eq!(gate_off.effective_pt, (5, 5), "gate off: base P/T only");
}

/// CR 701.21a: Demon of Wailing Agonies' granted combat-damage trigger makes
/// the damaged player sacrifice a creature OF THEIR CHOICE — the legal choices
/// must be exactly that player's creatures, not an auto-chosen single one.
#[test]
fn demon_of_wailing_agonies_damaged_player_sacrifices() {
    // Gate on.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let demon = scenario
            .add_creature_from_oracle(P0, "Demon of Wailing Agonies", 4, 4, DEMON)
            .id();
        scenario
            .add_creature(P0, "Test Commander", 1, 1)
            .commander();
        let mine = scenario.add_creature(P0, "Mine", 2, 2).id();
        let a = scenario.add_creature(P1, "Opp A", 2, 2).id();
        let b = scenario.add_creature(P1, "Opp B", 2, 2).id();
        let mut runner = scenario.build();

        let mut declared = false;
        let mut choice_cards: Option<Vec<ObjectId>> = None;
        for _ in 0..400 {
            let wf = runner.state().waiting_for.clone();
            let res = match wf {
                WaitingFor::DeclareAttackers { player, .. } if player == P0 && !declared => {
                    declared = true;
                    runner.act(GameAction::DeclareAttackers {
                        attacks: vec![(demon, AttackTarget::Player(P1))],
                        bands: vec![],
                    })
                }
                WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                    assignments: vec![],
                }),
                WaitingFor::EffectZoneChoice {
                    player, ref cards, ..
                } if player == P1 => {
                    choice_cards = Some(cards.clone());
                    runner.act(GameAction::SelectCards {
                        cards: vec![cards[0]],
                    })
                }
                WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
                _ => break,
            };
            if res.is_err() {
                break;
            }
            if choice_cards.is_some() {
                break;
            }
        }

        let cards =
            choice_cards.expect("Demon's combat damage must raise an EffectZoneChoice for P1");
        let mut sorted = cards.clone();
        sorted.sort();
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(
            sorted, expected,
            "the legal choices must be exactly P1's two creatures"
        );

        assert!(
            runner.state().battlefield.contains(&mine),
            "P0's creature is untouched by P1's own sacrifice"
        );
        assert!(!runner.state().battlefield.contains(&cards[0]));
        let p1_graveyard = &runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .expect("P1 exists")
            .graveyard;
        assert!(
            p1_graveyard.contains(&cards[0]),
            "the sacrificed creature must be in P1's graveyard, not merely off the battlefield"
        );
        let remaining = if cards[0] == a { b } else { a };
        assert!(runner.state().battlefield.contains(&remaining));
    }

    // Gate off: reach guard — damage was dealt (P1's life drops) — but no
    // granted trigger fires.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let demon = scenario
            .add_creature_from_oracle(P0, "Demon of Wailing Agonies", 4, 4, DEMON)
            .id();
        let mine = scenario.add_creature(P0, "Mine", 2, 2).id();
        let a = scenario.add_creature(P1, "Opp A", 2, 2).id();
        let b = scenario.add_creature(P1, "Opp B", 2, 2).id();
        let mut runner = scenario.build();

        let mut declared = false;
        let mut choice_seen = false;
        for _ in 0..400 {
            // Stop once combat has fully resolved into the second main phase —
            // this combat is the only thing under test, and letting the drive
            // loop run into a later turn (an empty-library draw step) is not.
            if declared && runner.state().phase == Phase::PostCombatMain {
                break;
            }
            let wf = runner.state().waiting_for.clone();
            let res = match wf {
                WaitingFor::DeclareAttackers { player, .. } if player == P0 && !declared => {
                    declared = true;
                    runner.act(GameAction::DeclareAttackers {
                        attacks: vec![(demon, AttackTarget::Player(P1))],
                        bands: vec![],
                    })
                }
                WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                    assignments: vec![],
                }),
                WaitingFor::EffectZoneChoice { .. } => {
                    choice_seen = true;
                    break;
                }
                WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
                _ => break,
            };
            if res.is_err() {
                break;
            }
        }

        assert!(
            !choice_seen,
            "gate off: the granted sacrifice trigger must not fire"
        );
        assert_eq!(
            runner.life(P1),
            16,
            "reach guard: P0's unboosted 4/4 must have dealt its combat damage"
        );
        assert!(runner.state().battlefield.contains(&mine));
        assert!(runner.state().battlefield.contains(&a));
        assert!(runner.state().battlefield.contains(&b));
    }
}

/// CR 603.3d + CR 601.2c: Tyrant's Familiar's granted attack trigger deals damage to a
/// target creature the DEFENDING player controls — in a three-player game the
/// legal targets must be exactly the attacked player's creatures, excluding
/// the uninvolved third player's.
#[test]
fn tyrants_familiar_attack_trigger_targets_defending_players_creature() {
    // Gate on.
    {
        let p2 = PlayerId(2);
        let mut scenario = GameScenario::new_n_player(3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let tyrant = scenario
            .add_creature_from_oracle(P0, "Tyrant's Familiar", 5, 5, TYRANT)
            .id();
        scenario
            .add_creature(P0, "Test Commander", 1, 1)
            .commander();
        let p1a = scenario.add_creature(P1, "P1 A", 1, 7).id();
        let p1b = scenario.add_creature(P1, "P1 B", 1, 7).id();
        let p2a = scenario.add_creature(p2, "P2 A", 1, 7).id();
        let p2b = scenario.add_creature(p2, "P2 B", 1, 7).id();
        let mut runner = scenario.build();

        let mut declared = false;
        let mut legal: Option<HashSet<ObjectId>> = None;
        for _ in 0..300 {
            if declared && runner.state().phase == Phase::DeclareBlockers {
                break;
            }
            let wf = runner.state().waiting_for.clone();
            let res = match wf {
                WaitingFor::DeclareAttackers { player, .. } if player == P0 && !declared => {
                    declared = true;
                    runner.act(GameAction::DeclareAttackers {
                        attacks: vec![(tyrant, AttackTarget::Player(P1))],
                        bands: vec![],
                    })
                }
                WaitingFor::TriggerTargetSelection {
                    ref target_slots, ..
                } => {
                    let slot = &target_slots[0];
                    legal = Some(
                        slot.legal_targets
                            .iter()
                            .filter_map(|t| match t {
                                TargetRef::Object(id) => Some(*id),
                                _ => None,
                            })
                            .collect(),
                    );
                    runner.choose_first_legal_target()
                }
                WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
                _ => break,
            };
            if res.is_err() {
                break;
            }
        }

        let legal = legal.expect("the attack must raise a TriggerTargetSelection");
        assert_eq!(
            legal,
            HashSet::from([p1a, p1b]),
            "legal targets must be exactly the attacked player's (P1's) creatures, excluding P2's"
        );

        let bf = &runner.state().battlefield;
        let p1_alive = [p1a, p1b].iter().filter(|id| bf.contains(id)).count();
        assert_eq!(
            p1_alive, 1,
            "exactly one of P1's creatures must have died to the 7 damage"
        );
        assert!(
            bf.contains(&p2a) && bf.contains(&p2b),
            "P2's creatures must be untouched"
        );
    }

    // Gate off: reach guard — the attack is declared — but no granted attack
    // trigger fires, so no TriggerTargetSelection is ever raised and all four
    // 1/7 creatures survive.
    {
        let p2 = PlayerId(2);
        let mut scenario = GameScenario::new_n_player(3, 7);
        scenario.at_phase(Phase::PreCombatMain);
        let tyrant = scenario
            .add_creature_from_oracle(P0, "Tyrant's Familiar", 5, 5, TYRANT)
            .id();
        let p1a = scenario.add_creature(P1, "P1 A", 1, 7).id();
        let p1b = scenario.add_creature(P1, "P1 B", 1, 7).id();
        let p2a = scenario.add_creature(p2, "P2 A", 1, 7).id();
        let p2b = scenario.add_creature(p2, "P2 B", 1, 7).id();
        let mut runner = scenario.build();

        let mut declared = false;
        let mut trigger_seen = false;
        for _ in 0..300 {
            if declared && runner.state().phase == Phase::PostCombatMain {
                break;
            }
            let wf = runner.state().waiting_for.clone();
            let res = match wf {
                WaitingFor::DeclareAttackers { player, .. } if player == P0 && !declared => {
                    declared = true;
                    runner.act(GameAction::DeclareAttackers {
                        attacks: vec![(tyrant, AttackTarget::Player(P1))],
                        bands: vec![],
                    })
                }
                WaitingFor::TriggerTargetSelection { .. } => {
                    trigger_seen = true;
                    break;
                }
                WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                    assignments: vec![],
                }),
                WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
                _ => break,
            };
            if res.is_err() {
                break;
            }
        }

        assert!(declared, "reach guard: the attack must have been declared");
        assert!(
            !trigger_seen,
            "gate off: the granted attack trigger must not fire"
        );
        let bf = &runner.state().battlefield;
        assert!(
            [p1a, p1b, p2a, p2b].iter().all(|id| bf.contains(id)),
            "gate off: all four 1/7 creatures must survive"
        );
    }
}

/// CR 702.121a: Skyhunter Strike Force's Lieutenant grant reaches OTHER
/// creatures you control (Melee), not itself — Skyhunter already has its own
/// unconditional Melee from its printed keyword line.
#[test]
fn skyhunter_strike_force_grants_melee_to_other_creatures() {
    // Gate on: the bear gains Melee and, attacking alone, becomes 3/3 at
    // DeclareBlockers (+1/+1 for the one opponent attacked this combat).
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature_from_oracle(P0, "Skyhunter Strike Force", 2, 2, SKYHUNTER);
        scenario
            .add_creature(P0, "Test Commander", 1, 1)
            .commander();
        let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let opp = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
        let mut runner = scenario.build();

        assert!(
            has_kw(&mut runner, bear, &Keyword::Melee),
            "gate on: other creatures you control have melee"
        );
        assert!(
            !has_kw(&mut runner, opp, &Keyword::Melee),
            "the opponent's creature must not gain melee"
        );

        runner.pass_both_players();
        runner
            .act(GameAction::DeclareAttackers {
                attacks: vec![(bear, AttackTarget::Player(P1))],
                bands: vec![],
            })
            .expect("DeclareAttackers should succeed");
        for _ in 0..20 {
            if runner.state().phase == Phase::DeclareBlockers {
                break;
            }
            if runner.act(GameAction::PassPriority).is_err() {
                break;
            }
        }
        assert_eq!(
            effective_pt(&mut runner, bear),
            (3, 3),
            "Melee: +1/+1 for the one opponent attacked this combat"
        );
    }

    // Gate off: the bear never gains melee and stays 2/2.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature_from_oracle(P0, "Skyhunter Strike Force", 2, 2, SKYHUNTER);
        let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let mut runner = scenario.build();
        assert!(!has_kw(&mut runner, bear, &Keyword::Melee));
        assert_eq!(effective_pt(&mut runner, bear), (2, 2));
    }
}

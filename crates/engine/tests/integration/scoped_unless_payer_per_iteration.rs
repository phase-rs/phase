//! Runtime regression for GitHub issue #7198 — a scope-bound unless-payer must
//! travel with the clause the `player_scope` fan-out iterates.
//!
//! CR 608.2f makes an "each opponent / each player … unless they …" instruction
//! resolve once per affected player, and CR 101.4 orders those choices APNAP.
//! CR 118.12a names the payer as the player the unless-clause names. So the
//! payer's identity is bound per fan-out iteration — it is `ScopedPlayer`,
//! resolved from `ResolvedAbility.scoped_player`, which only the fan-out writes.
//!
//! `extract_unless_pay_modifier` used to hoist that payment out of the effect
//! chain onto `TriggerDefinition.unless_pay`, which the runtime copies onto the
//! resolved trigger ROOT. That detaches the payment from the scoped clause, and
//! the root's `scoped_player` is then whatever unrelated event-context
//! machinery stamped it — the attacker's controller for an `AttackersDeclared`
//! event, the active player for a phase trigger, or nothing at all. Two harms
//! followed, both reproduced here:
//!
//!   1. **The root is not the scoped clause** (Rottenmouth Viper, whose scoped
//!      life-loss sits two links below a `PutCounter` root). On the attack route
//!      the CONTROLLER was prompted and offered their own permanents; on the ETB
//!      route nobody was prompted at all and the opponent simply lost the life.
//!   2. **The payer was never scope-bound** (Bellowing Mauler's `each player`,
//!      which fell through to `TriggeringPlayer`). One seat answered for the
//!      whole table, and if that seat paid, it paid with its own creature.
//!
//! These tests drive the real pipeline — verbatim Oracle text through the live
//! parser, real attack / end-step / upkeep triggers, and the production
//! `WaitingFor` prompts answered with real `GameAction`s.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::AbilityCost;
use engine::types::actions::{GameAction, UnlessCostBranch};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// Verbatim printed Oracle text. A paraphrase can take a different parser
/// branch and go green while the real card stays broken.
const ROTTENMOUTH_VIPER: &str = "As an additional cost to cast this spell, you may sacrifice any number of nonland permanents. This spell costs {1} less to cast for each permanent sacrificed this way.\nWhenever this creature enters or attacks, put a blight counter on it. Then for each blight counter on it, each opponent loses 4 life unless that player sacrifices a nonland permanent of their choice or discards a card.";

const BELLOWING_MAULER: &str = "At the beginning of your end step, each player loses 4 life unless they sacrifice a nontoken creature of their choice.";

const HAG_OF_CEASELESS_TORMENT: &str = "At the beginning of your upkeep, each opponent loses 3 life unless that player sacrifices a nonland permanent or discards a card.";

const BLIGHT: fn() -> CounterType = || CounterType::Generic("blight".to_string());

fn blight_counters(runner: &GameRunner, viper: ObjectId) -> u32 {
    runner.state().objects[&viper]
        .counters
        .get(&BLIGHT())
        .copied()
        .unwrap_or(0)
}

fn battlefield_ids(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.controller == player && o.zone == Zone::Battlefield)
        .map(|o| o.id)
        .collect()
}

/// Rottenmouth Viper on P0's battlefield attacking P1, with the attack trigger
/// already resolved up to whatever prompt it produced. Every opponent gets a
/// nonland permanent and a card in hand, so both branches of the disjunctive
/// cost are payable for them.
fn viper_attacking(player_count: u8) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(player_count, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let viper = scenario
        .add_creature_from_oracle(P0, "Rottenmouth Viper", 5, 4, ROTTENMOUTH_VIPER)
        .id();
    for seat in 1..player_count {
        let pid = PlayerId(seat);
        scenario.add_creature(pid, &format!("Grizzly Bears {seat}"), 2, 2);
        let card = format!("Filler Card {seat}");
        scenario.with_cards_in_hand(pid, &[card.as_str()]);
    }

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(viper, AttackTarget::Player(P1))])
        .expect("Viper attacks P1");
    runner.advance_until_stack_empty();
    (runner, viper)
}

/// Bellowing Mauler on P0's battlefield with its end-step trigger resolved up
/// to the first unless prompt. Both seats hold a nontoken creature, so the
/// `Sacrifice` cost is payable for either.
fn mauler_at_end_step() -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Bellowing Mauler", 4, 4, BELLOWING_MAULER);
    scenario.add_creature(P0, "Controller's Bear", 2, 2);
    scenario.add_creature(P1, "Opponent's Bear", 2, 2);

    let mut runner = scenario.build();
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    runner
}

/// CR 608.2f + CR 118.12a + CR 101.4: Viper's attack trigger taxes THE
/// OPPONENT, not its controller, and the opponent keeps their life until they
/// decline.
///
/// Hostile fixture: `stack.rs`'s `AttackersDeclared` handler stamps
/// `scoped_player` = the attacker's controller recursively over the whole chain
/// before resolution, so two authorities write the field the payer reads. The
/// fan-out's per-opponent rebind must win at the scoped node.
#[test]
fn viper_attack_trigger_offers_the_choice_to_each_opponent() {
    let (mut runner, viper) = viper_attacking(2);

    // Reach-guard: the trigger actually ran and its clause parsed into the full
    // disjunctive cost, so the assertions below are not about a trigger that
    // never fired or a clause that fell through to an unimplemented gap.
    assert_eq!(
        blight_counters(&runner, viper),
        1,
        "the attack trigger must have put a blight counter on Viper"
    );

    let (player, cost_count) = match &runner.state().waiting_for {
        WaitingFor::UnlessPaymentChooseCost { player, costs, .. } => (*player, costs.len()),
        other => panic!("expected a disjunctive unless prompt, got {other:?}"),
    };
    assert_eq!(
        player, P1,
        "CR 118.12a: the scoped opponent pays, not the controller"
    );
    assert_eq!(
        cost_count, 2,
        "the sacrifice-or-discard disjunction must reach the prompt"
    );

    // CR 118.12a: the life is lost only when the payment is declined.
    assert_eq!(runner.life(P1), 20, "P1 keeps their life at the prompt");
    assert_eq!(runner.life(P0), 20, "the controller is never the subject");

    runner
        .act(GameAction::ChooseUnlessCostBranch {
            choice: UnlessCostBranch::Decline,
        })
        .expect("declining the unless-cost must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(runner.life(P1), 16, "declining costs the opponent 4 life");
    assert_eq!(runner.life(P0), 20, "the controller loses nothing");
}

/// CR 118.12a: the cost's "of their choice" resolves against the PROMPTEE, so
/// the sacrifice candidates are the opponent's own permanents. Pre-fix the
/// controller was prompted and offered Viper and their own creatures.
#[test]
fn viper_attack_sacrifice_branch_offers_only_the_payers_own_permanents() {
    let (mut runner, _viper) = viper_attacking(2);

    let controller_permanents = battlefield_ids(&runner, P0);
    let opponent_permanents = battlefield_ids(&runner, P1);
    assert!(
        !controller_permanents.is_empty() && !opponent_permanents.is_empty(),
        "both seats must control a permanent for this to discriminate"
    );

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPaymentChooseCost { player: P1, .. }
        ),
        "the opponent must be the one choosing a branch, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseUnlessCostBranch {
            choice: UnlessCostBranch::Pay { index: 0 },
        })
        .expect("choosing the sacrifice branch must be accepted");

    match &runner.state().waiting_for {
        WaitingFor::WardSacrificeChoice {
            player, permanents, ..
        } => {
            assert_eq!(*player, P1, "the opponent picks what they sacrifice");
            for id in permanents {
                assert!(
                    opponent_permanents.contains(id),
                    "offered permanent {id:?} is not controlled by the payer"
                );
            }
            assert!(
                !permanents.is_empty(),
                "the payer's own nonland permanent must be offered"
            );
        }
        other => panic!("expected a sacrifice selection for the payer, got {other:?}"),
    }
}

/// CR 608.2f: Viper's ENTERS half behaves identically. This is the arm the live
/// report describes — pre-fix no prompt surfaced at all and the opponent simply
/// lost 4 life, because the root's `scoped_player` is unstamped on this route.
#[test]
fn viper_etb_trigger_offers_the_choice_to_each_opponent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let viper = scenario
        .add_creature_to_hand_from_oracle(P0, "Rottenmouth Viper", 5, 4, ROTTENMOUTH_VIPER)
        .id();
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    scenario.with_cards_in_hand(P1, &["Filler Card"]);
    let mut runner = scenario.build();

    let outcome = runner.cast(viper).free_cast().resolve();

    // Reach-guard: the enters trigger ran, so "P1 still has 20 life" cannot pass
    // vacuously on a trigger that never fired.
    let resolved_viper = outcome
        .find_object(|o| o.name == "Rottenmouth Viper" && o.zone == Zone::Battlefield)
        .expect("Viper resolves onto the battlefield");
    assert_eq!(
        outcome.state().objects[&resolved_viper]
            .counters
            .get(&BLIGHT())
            .copied()
            .unwrap_or(0),
        1,
        "the enters trigger must have put a blight counter on Viper"
    );

    match outcome.final_waiting_for() {
        WaitingFor::UnlessPaymentChooseCost { player, costs, .. } => {
            assert_eq!(*player, P1, "CR 118.12a: the scoped opponent is prompted");
            assert_eq!(costs.len(), 2, "the disjunctive cost must reach the prompt");
        }
        other => panic!("expected the opponent's unless prompt, got {other:?}"),
    }
    assert_eq!(
        outcome.life_delta(P1),
        0,
        "the life is lost only on a declined payment, never silently"
    );
    assert_eq!(outcome.life_delta(P0), 0, "the controller loses nothing");
}

/// CR 608.2f + CR 101.4: with three seats, EACH opponent is polled, in turn
/// order, and the controller is never polled at all.
#[test]
fn viper_three_player_polls_each_opponent_in_turn_order() {
    let (mut runner, _viper) = viper_attacking(3);

    match &runner.state().waiting_for {
        WaitingFor::UnlessPaymentChooseCost { player, .. } => {
            assert_eq!(*player, P1, "CR 101.4: the first opponent in turn order")
        }
        other => panic!("expected the first opponent's unless prompt, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseUnlessCostBranch {
            choice: UnlessCostBranch::Decline,
        })
        .expect("P1 declines");

    match &runner.state().waiting_for {
        WaitingFor::UnlessPaymentChooseCost { player, .. } => {
            assert_eq!(
                *player, P2,
                "CR 101.4: then the next opponent in turn order"
            )
        }
        other => panic!("expected the second opponent's unless prompt, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseUnlessCostBranch {
            choice: UnlessCostBranch::Decline,
        })
        .expect("P2 declines");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        20,
        "the controller is never polled or taxed"
    );
    assert_eq!(runner.life(P1), 16, "the first opponent declined");
    assert_eq!(runner.life(P2), 16, "the second opponent declined");
}

/// CR 608.2f: the seats resolve INDEPENDENTLY — one opponent paying does not
/// spare the other, and each pays out of their own permanents.
#[test]
fn viper_three_player_one_opponent_pays_while_the_other_declines() {
    let (mut runner, _viper) = viper_attacking(3);
    let p1_permanents = battlefield_ids(&runner, P1);

    runner
        .act(GameAction::ChooseUnlessCostBranch {
            choice: UnlessCostBranch::Pay { index: 0 },
        })
        .expect("P1 chooses the sacrifice branch");
    let sacrificed = match &runner.state().waiting_for {
        WaitingFor::WardSacrificeChoice {
            player, permanents, ..
        } => {
            assert_eq!(*player, P1, "P1 picks from P1's own permanents");
            *permanents
                .first()
                .expect("P1 has a sacrificeable permanent")
        }
        other => panic!("expected P1's sacrifice selection, got {other:?}"),
    };
    assert!(
        p1_permanents.contains(&sacrificed),
        "the sacrificed permanent must be P1's own"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![sacrificed],
        })
        .expect("P1 sacrifices the chosen permanent");

    match &runner.state().waiting_for {
        WaitingFor::UnlessPaymentChooseCost { player, .. } => {
            assert_eq!(*player, P2, "P2 is still polled independently")
        }
        other => panic!("expected P2's unless prompt, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseUnlessCostBranch {
            choice: UnlessCostBranch::Decline,
        })
        .expect("P2 declines");
    runner.advance_until_stack_empty();

    assert_eq!(runner.life(P0), 20, "the controller is untouched");
    assert_eq!(runner.life(P1), 20, "paying spares P1 the life loss");
    assert_eq!(runner.life(P2), 16, "declining costs P2 the life");
    assert_eq!(
        runner.state().objects[&sacrificed].zone,
        Zone::Graveyard,
        "P1's payment actually sacrificed their permanent"
    );
}

/// CR 608.2f + CR 101.4 (issue #7198): Bellowing Mauler's `each player` subject
/// is scope-bearing too, so BOTH seats are polled in APNAP order and each is
/// offered only their own creature. Pre-fix the payer fell through to
/// `TriggeringPlayer` and one seat answered twice with its own permanents.
#[test]
fn bellowing_mauler_end_step_offers_each_player_their_own_choice() {
    let mut runner = mauler_at_end_step();
    let p1_permanents = battlefield_ids(&runner, P1);

    // Reach-guard: the clause parsed into a real sacrifice cost rather than an
    // unimplemented gap, so a missing prompt below would be a routing failure.
    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => {
            assert_eq!(*player, P0, "CR 101.4: the active player chooses first");
            assert!(
                matches!(cost, AbilityCost::Sacrifice(_)),
                "the nontoken-creature sacrifice must reach the prompt, got {cost:?}"
            );
        }
        other => panic!("expected the active player's unless prompt, got {other:?}"),
    }

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P0 declines");

    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { player, .. } => assert_eq!(
            *player, P1,
            "CR 608.2f: the nonactive player is polled separately, not skipped"
        ),
        other => panic!("expected the nonactive player's unless prompt, got {other:?}"),
    }
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1 pays");

    // Cost-side identity: "a nontoken creature of their choice" resolves against
    // the promptee. Pre-fix this list held the controller's Mauler and Bear.
    let sacrificed = match &runner.state().waiting_for {
        WaitingFor::WardSacrificeChoice {
            player, permanents, ..
        } => {
            assert_eq!(*player, P1, "P1 chooses what P1 sacrifices");
            for id in permanents {
                assert!(
                    p1_permanents.contains(id),
                    "offered permanent {id:?} is not controlled by the payer"
                );
            }
            *permanents.first().expect("P1 has a nontoken creature")
        }
        other => panic!("expected P1's sacrifice selection, got {other:?}"),
    };
    runner
        .act(GameAction::SelectCards {
            cards: vec![sacrificed],
        })
        .expect("P1 sacrifices their own creature");
    runner.advance_until_stack_empty();

    assert_eq!(runner.life(P0), 16, "P0 declined and lost the life");
    assert_eq!(runner.life(P1), 20, "P1 paid and kept their life");
    assert_eq!(
        runner.state().objects[&sacrificed].zone,
        Zone::Graveyard,
        "P1's payment actually sacrificed their creature"
    );
}

/// Regression control for the relocating cards (bucket A2): Hag's scoped clause
/// IS its chain root, so the hoist was already harmless for it. Relocating the
/// modifier onto that same node must not change what it does.
#[test]
fn hag_of_ceaseless_torment_upkeep_still_prompts_the_opponent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_creature_from_oracle(
        P0,
        "Hag of Ceaseless Torment",
        3,
        3,
        HAG_OF_CEASELESS_TORMENT,
    );
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    scenario.with_cards_in_hand(P1, &["Filler Card"]);
    let mut runner = scenario.build();

    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    match &runner.state().waiting_for {
        WaitingFor::UnlessPaymentChooseCost { player, costs, .. } => {
            assert_eq!(*player, P1, "the scoped opponent is prompted, as before");
            assert_eq!(costs.len(), 2, "the disjunctive cost survives relocation");
        }
        other => panic!("expected the opponent's unless prompt, got {other:?}"),
    }
    assert_eq!(runner.life(P0), 20, "the controller is untouched");
    assert_eq!(runner.life(P1), 20, "the life is lost only on a decline");
}

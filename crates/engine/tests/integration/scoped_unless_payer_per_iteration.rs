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
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::AbilityCost;
use engine::types::actions::{GameAction, UnlessCostBranch};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::{
    GameState, PendingSimultaneousUnless, PersistedGameState, PersistedRestoreFinalization,
    SimultaneousUnlessAnswer, SimultaneousUnlessDelivery, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaSourceSelection};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

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
    viper_attacking_with_blight_counters(player_count, 0)
}

fn viper_attacking_with_blight_counters(
    player_count: u8,
    starting_blight_counters: u32,
) -> (GameRunner, ObjectId) {
    let (scenario, viper) = viper_board(player_count, starting_blight_counters);
    (viper_attacks(scenario, viper), viper)
}

/// Rottenmouth Viper on P0's battlefield with `starting_blight_counters`, and
/// no other permanent or card for anyone.
fn bare_viper_board(player_count: u8, starting_blight_counters: u32) -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new_n_player(player_count, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let viper = scenario
        .add_creature_from_oracle(P0, "Rottenmouth Viper", 6, 6, ROTTENMOUTH_VIPER)
        .id();
    if starting_blight_counters > 0 {
        scenario.with_counter(viper, BLIGHT(), starting_blight_counters);
    }
    (scenario, viper)
}

/// `bare_viper_board` with, for every opponent seat, a Grizzly Bears named
/// "Grizzly Bears {seat}" and a card named "Filler Card {seat}" in hand.
fn viper_board(player_count: u8, starting_blight_counters: u32) -> (GameScenario, ObjectId) {
    let (mut scenario, viper) = bare_viper_board(player_count, starting_blight_counters);
    for seat in 1..player_count {
        let pid = PlayerId(seat);
        scenario.add_creature(pid, &format!("Grizzly Bears {seat}"), 2, 2);
        let card = format!("Filler Card {seat}");
        scenario.with_cards_in_hand(pid, &[card.as_str()]);
    }
    (scenario, viper)
}

/// Build `scenario`, attack P1 with the Viper and resolve its attack trigger up
/// to whatever prompt it produces.
fn viper_attacks(scenario: GameScenario, viper: ObjectId) -> GameRunner {
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(viper, AttackTarget::Player(P1))])
        .expect("Viper attacks P1");
    runner.advance_until_stack_empty();
    runner
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

/// CR 608.2c + CR 608.2h + CR 101.4: the preceding instruction adds a blight
/// counter, then the repeat count is determined from Viper's current counters.
/// Starting with one counter makes the attack trigger's second counter
/// observable: P1 answers two prompts, losing no life until both are in.
///
/// The ruling's order: "While resolving Rottenmouth Viper's triggered ability,
/// your opponent chooses a card to be discarded without revealing it, chooses a
/// nonland permanent to be sacrificed, or chooses to do neither. … Then that
/// player repeats that process if it hasn't yet been done once for each blight
/// counter on Rottenmouth Viper. Finally, that player discards the chosen
/// cards, sacrifices the chosen permanents, and loses 4 life the appropriate
/// number of times." So both prompts precede any life loss, and the trigger is
/// still the resolving object (CR 608.2c + CR 117.2e) at each of them.
#[test]
fn viper_attack_repeats_the_unless_clause_for_each_blight_counter() {
    let (mut runner, viper) = viper_attacking_with_blight_counters(2, 1);

    assert_eq!(
        blight_counters(&runner, viper),
        2,
        "the attack trigger adds a counter before measuring the repeat count"
    );
    for iteration in 0..2 {
        match &runner.state().waiting_for {
            WaitingFor::UnlessPaymentChooseCost { player, costs, .. } => {
                assert_eq!(*player, P1, "each instance is scoped to the opponent");
                assert_eq!(costs.len(), 2, "both unless-cost branches remain available");
            }
            other => panic!("expected unless prompt {iteration}, got {other:?}"),
        }
        assert_eq!(
            runner.life(P1),
            20,
            "CR 101.4: every choice precedes any life loss (prompt {iteration})"
        );
        assert_poll_holds_its_resolution(&runner, viper, &format!("prompt {iteration}"));
        // No `advance_until_stack_empty` between the answers: the second prompt
        // is raised by the first answer itself.
        decline(&mut runner);
    }

    assert_eq!(runner.life(P1), 12, "two counters cause two 4-life losses");
    // A fanned-out instance would tax P1's own opponent, the controller.
    assert_eq!(runner.life(P0), 20, "the controller is not taxed");
    assert!(
        runner.state().stack.is_empty(),
        "the trigger fully resolves"
    );
    assert_poll_released(&runner, P0, "after the second decline");
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
        .add_creature_to_hand_from_oracle(P0, "Rottenmouth Viper", 6, 6, ROTTENMOUTH_VIPER)
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
    drop(outcome);

    // CR 101.4: the enters trigger runs the same choose-all-then-act
    // poll, which keeps the trigger's own `ZoneChanged` event for its delivery.
    assert!(
        matches!(
            poll(&runner).and_then(|carrier| carrier.trigger_event.as_ref()),
            Some(GameEvent::ZoneChanged { .. })
        ),
        "the poll captured the enters event: {:?}",
        poll(&runner).map(|carrier| &carrier.trigger_event)
    );
    assert_poll_holds_its_resolution(&runner, resolved_viper, "P1's prompt");
    decline(&mut runner);
    assert_eq!(runner.life(P1), 16, "the declined instance is delivered");
    assert_eq!(runner.life(P0), 20);
    assert_poll_released(&runner, P0, "after the decline");
}

/// CR 608.2f + CR 101.4: with three seats, EACH opponent is polled, in turn
/// order, and the controller is never polled at all. Every prompt precedes
/// every action, and the trigger is still resolving at the last one.
#[test]
fn viper_three_player_polls_each_opponent_in_turn_order() {
    let (mut runner, viper) = viper_attacking(3);

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
    assert_eq!(
        runner.life(P1),
        20,
        "CR 101.4: P1's decline has not cost life while P2 chooses"
    );
    assert_poll_holds_its_resolution(&runner, viper, "P2's prompt");
    runner
        .act(GameAction::ChooseUnlessCostBranch {
            choice: UnlessCostBranch::Decline,
        })
        .expect("P2 declines");
    assert_poll_released(&runner, P0, "after the last decline");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        20,
        "the controller is never polled or taxed"
    );
    assert_eq!(runner.life(P1), 16, "the first opponent declined");
    assert_eq!(runner.life(P2), 16, "the second opponent declined");
}

/// CR 608.2f + CR 101.4: the seats answer INDEPENDENTLY, in turn order, each
/// finishing every instance before the next seat starts; one opponent paying
/// does not spare the other, and every chosen action happens together at the
/// end ("each opponent in turn order makes all of their choices, then all of
/// the actions occur simultaneously").
#[test]
fn viper_three_player_one_opponent_pays_while_the_other_declines() {
    let (mut runner, viper) = viper_attacking_with_blight_counters(3, 1);
    let bears = object_named(&runner, "Grizzly Bears 1");
    let card = object_named(&runner, "Filler Card 1");
    let mut order = Vec::new();

    order.push(prompted(&runner));
    choose_branch(&mut runner, Branch::Sacrifice);
    order.push(prompted(&runner));
    pick(&mut runner, bears);
    order.push(prompted(&runner));
    choose_branch(&mut runner, Branch::Discard);
    order.push(prompted(&runner));
    pick(&mut runner, card);

    for instance in 0..2 {
        order.push(prompted(&runner));
        assert_eq!(
            runner.state().objects[&bears].zone,
            Zone::Battlefield,
            "CR 101.4: P1's chosen permanent stays until every seat has answered"
        );
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Hand,
            "CR 101.4a: P1's chosen card stays in hand until every seat has answered"
        );
        if instance == 0 {
            assert_poll_holds_its_resolution(&runner, viper, "P2's first prompt");
        }
        decline(&mut runner);
    }
    assert_eq!(
        order,
        vec![P1, P1, P1, P1, P2, P2],
        "CR 101.4: P1 answers both instances (branch, pick, branch, pick) before P2"
    );

    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&card].zone, Zone::Graveyard);
    assert_eq!(runner.life(P0), 20, "the controller is untouched");
    assert_eq!(runner.life(P1), 20, "paying both instances spares P1");
    assert_eq!(runner.life(P2), 12, "declining both costs P2 8 life");
    assert_poll_released(&runner, P0, "after P2's last decline");
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

/// Ashnod's Altar, verbatim Oracle text: a mana ability whose cost is a
/// sacrifice, so activating it would consume a permanent.
const ASHNODS_ALTAR: &str = "Sacrifice a creature: Add {C}{C}.";

/// Every mana action the legal-action lists offer, flat or per object: what the
/// AI chooses from and what the client's mana ring paints.
fn offered_mana_actions(runner: &GameRunner) -> Vec<GameAction> {
    let (flat, _, by_object) = engine::ai_support::legal_actions_full(runner.state());
    flat.iter()
        .chain(by_object.values().flatten())
        .filter(|action| {
            matches!(
                action,
                GameAction::TapLandForMana { .. }
                    | GameAction::UntapLandForMana { .. }
                    | GameAction::ActivateAbility { .. }
            )
        })
        .cloned()
        .collect()
}

/// Reach guard: `land`'s mana ability is activatable for `player`, so a refused
/// tap of it is the prompt's refusal, not a missing source.
fn land_mana_selection(
    runner: &GameRunner,
    player: PlayerId,
    land: ObjectId,
) -> ManaSourceSelection {
    engine::game::mana_sources::activatable_mana_source_selections(runner.state(), player)
        .into_iter()
        .find(|selection| selection.source.object_id == land)
        .expect("reach guard: the land's mana ability is activatable")
}

fn activate_altar(altar: ObjectId) -> GameAction {
    GameAction::ActivateAbility {
        source_id: altar,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    }
}

/// CR 605.3a + CR 608.2g: submits each mana action at the open prompt and
/// returns its refusal. Nothing moves: the prompt, `payer`'s pool, `land` and
/// every one of `permanents` are as they were before the attempts.
fn submit_refused_mana_actions(
    runner: &mut GameRunner,
    payer: PlayerId,
    land: ObjectId,
    permanents: &[ObjectId],
    actions: Vec<GameAction>,
) -> Vec<EngineError> {
    let prompt = runner.state().waiting_for.clone();
    let refusals = actions
        .into_iter()
        .map(|action| match runner.act(action.clone()) {
            Ok(_) => panic!("{action:?} must be refused at {prompt:?}"),
            Err(refusal) => refusal,
        })
        .collect();
    assert_eq!(
        runner.state().waiting_for,
        prompt,
        "the refused actions leave the prompt open"
    );
    assert_eq!(
        runner.state().players[payer.0 as usize].mana_pool.total(),
        0,
        "no mana was produced"
    );
    assert!(
        !runner.state().objects[&land].tapped,
        "the land is untapped"
    );
    for permanent in permanents {
        assert_eq!(
            runner.state().objects[permanent].zone,
            Zone::Battlefield,
            "{permanent:?} was not consumed"
        );
    }
    refusals
}

fn names_the_manaless_payment(refusal: &EngineError) -> bool {
    matches!(refusal, EngineError::InvalidAction(reason) if reason.contains("asks for no mana"))
}

/// CR 605.3a + CR 117.1d + CR 608.2g: Bellowing Mauler's "each player loses 4
/// life unless they sacrifice a nontoken creature of their choice" asks for a
/// sacrifice (CR 701.21a), not for mana, so neither payer may activate a mana
/// ability at their prompt: Ashnod's Altar ("Sacrifice a creature: Add
/// {C}{C}.") and a Swamp are refused and offered by no legal-action list. The
/// payers are asked in APNAP order (CR 101.4), and each payment then completes.
#[test]
fn bellowing_mauler_prompt_opens_no_mana_window() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Bellowing Mauler", 4, 6, BELLOWING_MAULER);
    let p0_bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let altar = scenario
        .add_artifact_from_oracle(P0, "Ashnod's Altar", ASHNODS_ALTAR)
        .id();
    let p0_swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let p1_bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let p1_swamp = scenario.add_basic_land(P1, ManaColor::Black);
    let mut runner = scenario.build();
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::UnlessPayment {
                player: P0,
                cost: AbilityCost::Sacrifice(_),
                ..
            }
        ),
        "reach guard: the active player is asked to sacrifice: {:?}",
        runner.state().waiting_for
    );
    let offered = offered_mana_actions(&runner);
    assert!(
        offered.is_empty(),
        "no mana action is offered at a sacrifice payment: {offered:?}"
    );
    let selection = land_mana_selection(&runner, P0, p0_swamp);
    let refusals = submit_refused_mana_actions(
        &mut runner,
        P0,
        p0_swamp,
        &[p0_bears, altar],
        vec![
            activate_altar(altar),
            GameAction::TapLandForMana { selection },
        ],
    );
    for refusal in &refusals {
        assert!(
            names_the_manaless_payment(refusal),
            "CR 605.3a refuses the mana action: {refusal:?}"
        );
    }

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P0 chooses to sacrifice");
    match &runner.state().waiting_for {
        WaitingFor::WardSacrificeChoice {
            player, permanents, ..
        } => {
            assert_eq!(*player, P0, "P0 picks what P0 sacrifices");
            assert!(
                permanents.contains(&p0_bears),
                "P0's Grizzly Bears is offered"
            );
        }
        other => panic!("expected P0's sacrifice selection, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![p0_bears],
        })
        .expect("P0 sacrifices Grizzly Bears");

    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::UnlessPayment {
                player: P1,
                cost: AbilityCost::Sacrifice(_),
                ..
            }
        ),
        "reach guard: the nonactive player is asked next: {:?}",
        runner.state().waiting_for
    );
    let offered = offered_mana_actions(&runner);
    assert!(
        offered.is_empty(),
        "no mana action is offered to the second payer: {offered:?}"
    );
    let selection = land_mana_selection(&runner, P1, p1_swamp);
    let refusals = submit_refused_mana_actions(
        &mut runner,
        P1,
        p1_swamp,
        &[p1_bears],
        vec![GameAction::TapLandForMana { selection }],
    );
    for refusal in &refusals {
        assert!(
            names_the_manaless_payment(refusal),
            "CR 605.3a refuses the second payer's tap: {refusal:?}"
        );
    }
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("P1 declines");
    runner.advance_until_stack_empty();

    assert_eq!(runner.life(P0), 20, "P0 paid and kept their life");
    assert_eq!(runner.life(P1), 16, "P1 declined and lost 4 life");
    assert_eq!(
        runner.state().objects[&p0_bears].zone,
        Zone::Graveyard,
        "P0's payment sacrificed Grizzly Bears"
    );
    assert!(runner.state().stack.is_empty(), "the trigger resolved");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the game returns to priority: {:?}",
        runner.state().waiting_for
    );
}

/// Rottenmouth Viper attacking P1 with no blight counter before the attack, so
/// its trigger repeats once. P1 controls Grizzly Bears, Ashnod's Altar and a
/// Swamp and holds one card, so both branches are payable and a sacrifice-cost
/// mana ability and a land are on hand. Returns the runner at P1's branch
/// choice with the Bears, the Altar, the Swamp and the card.
fn viper_attacking_into_mana_sources() -> (GameRunner, [ObjectId; 4]) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let viper = scenario
        .add_creature_from_oracle(P0, "Rottenmouth Viper", 6, 6, ROTTENMOUTH_VIPER)
        .id();
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let altar = scenario
        .add_artifact_from_oracle(P1, "Ashnod's Altar", ASHNODS_ALTAR)
        .id();
    let swamp = scenario.add_basic_land(P1, ManaColor::Black);
    let card = scenario.add_card_to_hand(P1, "Filler Card");
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(viper, AttackTarget::Player(P1))])
        .expect("Viper attacks P1");
    runner.advance_until_stack_empty();
    (runner, [bears, altar, swamp, card])
}

/// CR 605.3a + CR 608.2g + CR 118.12a: Rottenmouth Viper's "each opponent
/// loses 4 life unless that player sacrifices a nonland permanent of their
/// choice or discards a card" asks for no mana at any of its prompts: neither
/// at the choice of which cost to pay nor at the choice of the permanent to
/// sacrifice (CR 701.21a) or the card to discard may P1 activate Ashnod's Altar
/// or tap a Swamp. Each branch is then paid in full.
#[test]
fn viper_branch_and_pick_prompts_open_no_mana_window() {
    for branch in [0usize, 1] {
        let (mut runner, [bears, altar, swamp, card]) = viper_attacking_into_mana_sources();
        let p1_permanents = [bears, altar];
        assert!(
            matches!(
                &runner.state().waiting_for,
                WaitingFor::UnlessPaymentChooseCost { player: P1, costs, .. } if costs.len() == 2
            ),
            "reach guard: P1 chooses between the sacrifice and the discard: {:?}",
            runner.state().waiting_for
        );

        let offered = offered_mana_actions(&runner);
        assert!(
            offered.is_empty(),
            "branch {branch}: no mana action is offered at the branch choice: {offered:?}"
        );
        let selection = land_mana_selection(&runner, P1, swamp);
        submit_refused_mana_actions(
            &mut runner,
            P1,
            swamp,
            &p1_permanents,
            vec![
                activate_altar(altar),
                GameAction::TapLandForMana { selection },
            ],
        );

        runner
            .act(GameAction::ChooseUnlessCostBranch {
                choice: UnlessCostBranch::Pay { index: branch },
            })
            .expect("P1 chooses a branch");
        let picked = match (&runner.state().waiting_for, branch) {
            (
                WaitingFor::WardSacrificeChoice {
                    player, permanents, ..
                },
                0,
            ) => {
                assert_eq!(*player, P1, "P1 picks what P1 sacrifices");
                assert!(
                    permanents.contains(&bears) && permanents.contains(&altar),
                    "the Bears and the Altar are offered: {permanents:?}"
                );
                bears
            }
            (WaitingFor::WardDiscardChoice { player, cards, .. }, 1) => {
                assert_eq!(*player, P1, "P1 picks what P1 discards");
                assert!(cards.contains(&card), "the card in hand is offered");
                card
            }
            (other, _) => panic!("branch {branch}: expected P1's pick prompt, got {other:?}"),
        };

        let offered = offered_mana_actions(&runner);
        assert!(
            offered.is_empty(),
            "branch {branch}: no mana action is offered at the pick: {offered:?}"
        );
        let selection = land_mana_selection(&runner, P1, swamp);
        submit_refused_mana_actions(
            &mut runner,
            P1,
            swamp,
            &p1_permanents,
            vec![
                activate_altar(altar),
                GameAction::TapLandForMana { selection },
            ],
        );

        runner
            .act(GameAction::SelectCards {
                cards: vec![picked],
            })
            .expect("P1 completes the payment");
        runner.advance_until_stack_empty();

        assert_eq!(
            runner.state().objects[&picked].zone,
            Zone::Graveyard,
            "branch {branch}: the picked object was paid"
        );
        assert_eq!(runner.life(P1), 20, "branch {branch}: paying spares P1");
        assert!(runner.state().stack.is_empty(), "the trigger resolved");
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P0 }
            ),
            "branch {branch}: P0 gets priority back: {:?}",
            runner.state().waiting_for
        );
    }
}

// ---------------------------------------------------------------------------
// CR 101.4 choose-all-then-act: Rottenmouth Viper's poll
// ---------------------------------------------------------------------------

const P3: PlayerId = PlayerId(3);

/// Verbatim Oracle texts of the cards the integration fixture does not carry.
const FIERY_TEMPER: &str = "Fiery Temper deals 3 damage to any target.\nMadness {R} (If you discard this card, discard it into exile. When you do, cast it for its madness cost or put it into your graveyard.)";
const MIND_CONTROL: &str = "Enchant creature\nYou control enchanted creature.";
const ABYSSAL_PERSECUTOR: &str =
    "Flying, trample\nYou can't win the game and your opponents can't lose the game.";

fn poll(runner: &GameRunner) -> Option<&PendingSimultaneousUnless> {
    runner.state().pending_simultaneous_unless.as_deref()
}

/// The delivery cursor of a poll whose answers are being performed.
fn delivery(runner: &GameRunner) -> Option<&SimultaneousUnlessDelivery> {
    poll(runner).and_then(PendingSimultaneousUnless::delivery)
}

fn object_named(runner: &GameRunner, name: &str) -> ObjectId {
    let mut found = runner
        .state()
        .objects
        .values()
        .filter(|object| object.name == name)
        .map(|object| object.id);
    let id = found
        .next()
        .unwrap_or_else(|| panic!("no object is named {name}"));
    assert!(found.next().is_none(), "{name} names one object");
    id
}

fn zone_of(runner: &GameRunner, object: ObjectId) -> Zone {
    runner.state().objects[&object].zone
}

/// Stack entries whose source is `source`.
fn stack_entries_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count()
}

/// Queued triggered abilities whose source is `source`.
fn deferred_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .deferred_triggers
        .iter()
        .filter(|context| context.pending.source_id == source)
        .count()
}

/// CR 608.2c + CR 117.2e: the Viper's trigger is still the resolving object
/// while its poll collects: it is the poll's bound entry, nothing is on the
/// stack, and no player has priority.
fn assert_poll_holds_its_resolution(runner: &GameRunner, viper: ObjectId, context: &str) {
    let carrier = poll(runner).unwrap_or_else(|| panic!("{context}: the poll is open"));
    let entry = runner
        .state()
        .resolving_stack_entry
        .as_ref()
        .unwrap_or_else(|| panic!("{context}: the Viper trigger is still resolving"));
    assert_eq!(
        entry.source_id, viper,
        "{context}: the resolving entry is the Viper's trigger"
    );
    assert_eq!(
        carrier.resolving_entry,
        Some(entry.id),
        "{context}: the poll is bound to that entry"
    );
    assert!(
        runner.state().stack.is_empty(),
        "{context}: nothing is put on the stack while the trigger resolves"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{context}: no player has priority while the trigger resolves"
    );
}

/// CR 608.2n + CR 117.3b: the poll was delivered, the trigger has left the
/// stack, and `player` has priority.
fn assert_poll_released(runner: &GameRunner, player: PlayerId, context: &str) {
    assert!(poll(runner).is_none(), "{context}: the poll is closed");
    assert!(
        runner.state().resolving_stack_entry.is_none(),
        "{context}: the trigger has left the stack"
    );
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player },
        "{context}: priority goes to the active player"
    );
}

/// The player answering the open unless or pick prompt.
fn prompted(runner: &GameRunner) -> PlayerId {
    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { player, .. }
        | WaitingFor::UnlessPaymentChooseCost { player, .. }
        | WaitingFor::WardSacrificeChoice { player, .. }
        | WaitingFor::WardDiscardChoice { player, .. } => *player,
        other => panic!("expected an unless or pick prompt, got {other:?}"),
    }
}

/// CR 118.12a: decline the open unless prompt.
fn decline(runner: &mut GameRunner) {
    let action = match &runner.state().waiting_for {
        WaitingFor::UnlessPaymentChooseCost { .. } => GameAction::ChooseUnlessCostBranch {
            choice: UnlessCostBranch::Decline,
        },
        WaitingFor::UnlessPayment { .. } => GameAction::PayUnlessCost { pay: false },
        other => panic!("expected an unless prompt to decline, got {other:?}"),
    };
    runner.act(action).expect("the decline is accepted");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Branch {
    Sacrifice,
    Discard,
}

impl Branch {
    fn names(self, cost: &AbilityCost) -> bool {
        match self {
            Branch::Sacrifice => matches!(cost, AbilityCost::Sacrifice(_)),
            Branch::Discard => matches!(cost, AbilityCost::Discard { .. }),
        }
    }
}

/// CR 118.12a: choose to pay `branch` at the open unless prompt, which opens
/// that branch's pick.
fn choose_branch(runner: &mut GameRunner, branch: Branch) {
    let action = match &runner.state().waiting_for {
        WaitingFor::UnlessPaymentChooseCost { costs, .. } => {
            let index = costs
                .iter()
                .position(|cost| branch.names(cost))
                .unwrap_or_else(|| panic!("{branch:?} is offered: {costs:?}"));
            GameAction::ChooseUnlessCostBranch {
                choice: UnlessCostBranch::Pay { index },
            }
        }
        WaitingFor::UnlessPayment { cost, .. } => {
            assert!(
                branch.names(cost),
                "{branch:?} is the offered cost: {cost:?}"
            );
            GameAction::PayUnlessCost { pay: true }
        }
        other => panic!("expected an unless prompt, got {other:?}"),
    };
    runner.act(action).expect("choosing a branch is accepted");
}

/// Pick `object` at the open pick prompt.
fn pick(runner: &mut GameRunner, object: ObjectId) {
    runner
        .act(GameAction::SelectCards {
            cards: vec![object],
        })
        .expect("the pick is accepted");
}

/// Pass priority while it is a priority wait, so the stack's top object
/// resolves up to the first prompt it raises (no trigger-ordering shortcut).
fn pass_until_prompt(runner: &mut GameRunner) {
    for _ in 0..16 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("the pass is accepted");
    }
    panic!("no prompt was raised: {:?}", runner.state().waiting_for);
}

/// Build `scenario`, attack P1 with the Viper and pass until its trigger
/// raises its first prompt (or a CR 616.1 choice).
fn viper_attacks_without_advancing(scenario: GameScenario, viper: ObjectId) -> GameRunner {
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(viper, AttackTarget::Player(P1))])
        .expect("Viper attacks P1");
    pass_until_prompt(&mut runner);
    runner
}

fn concede(runner: &mut GameRunner, player: PlayerId) {
    runner
        .act(GameAction::Concede { player_id: player })
        .expect("a concession is accepted");
}

fn is_eliminated(runner: &GameRunner, player: PlayerId) -> bool {
    runner.state().players[player.0 as usize].is_eliminated
}

/// Save and restore through persistence: capture, decode, admit, finalize.
fn round_trip(state: &GameState) -> GameState {
    let json = serde_json::to_string(&PersistedGameState::capture(state.clone()))
        .expect("capture: the state serializes");
    serde_json::from_str::<PersistedGameState>(&json)
        .expect("decode: the persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("admit: the persisted state is admissible")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("finalize: the restored state is publishable")
}

/// V1. CR 101.4 + CR 118.12a: an object chosen for an earlier instance can't be
/// chosen again ("Cards or permanents they've chosen previously during this
/// process can't be chosen again"), and nothing moves until the last answer.
#[test]
fn viper_picks_stay_put_until_the_last_answer_and_are_not_offered_twice() {
    let (mut runner, viper) = viper_attacking_with_blight_counters(2, 1);
    let bears = object_named(&runner, "Grizzly Bears 1");
    let card = object_named(&runner, "Filler Card 1");

    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, bears);

    // The second instance: P1's only nonland permanent is already chosen, so
    // only the discard branch is offered.
    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => {
            assert_eq!(*player, P1);
            assert!(
                matches!(cost, AbilityCost::Discard { .. }),
                "the sacrifice branch's pool excludes the chosen Bears: {cost:?}"
            );
        }
        other => panic!("expected P1's second prompt, got {other:?}"),
    }
    choose_branch(&mut runner, Branch::Discard);
    match &runner.state().waiting_for {
        WaitingFor::WardDiscardChoice { player, cards, .. } => {
            assert_eq!(*player, P1);
            assert_eq!(cards, &vec![card], "reach guard: the second pick is open");
        }
        other => panic!("expected P1's discard pick, got {other:?}"),
    }
    assert_eq!(zone_of(&runner, bears), Zone::Battlefield);
    assert_eq!(zone_of(&runner, card), Zone::Hand);
    assert_poll_holds_its_resolution(&runner, viper, "the second pick");

    pick(&mut runner, card);
    assert_eq!(zone_of(&runner, bears), Zone::Graveyard);
    assert_eq!(zone_of(&runner, card), Zone::Graveyard);
    assert_eq!(runner.life(P1), 20, "both instances were paid");
    assert_eq!(runner.life(P0), 20);
    assert_poll_released(&runner, P0, "after the last pick");
}

/// V1b. CR 603.2 + CR 704.3: a last answer that is a decline settles the poll
/// through the answer's own post-action pipeline: both instances' life losses
/// trigger Exquisite Blood ("Whenever an opponent loses life, you gain that
/// much life.") once each.
#[test]
fn viper_last_decline_puts_each_life_loss_trigger_on_the_stack() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(2, 1);
    let blood = scenario.add_real_card(P0, "Exquisite Blood", Zone::Battlefield, db);
    let mut runner = viper_attacks(scenario, viper);

    decline(&mut runner);
    assert_eq!(
        stack_entries_from(&runner, blood),
        0,
        "nothing triggered yet"
    );
    decline(&mut runner);

    assert_eq!(runner.life(P1), 12);
    assert_eq!(
        stack_entries_from(&runner, blood),
        2,
        "CR 603.2: one trigger per life-loss event"
    );
    assert!(poll(&runner).is_none());
    assert!(runner.state().resolving_stack_entry.is_none());
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), 28);
}

/// V1c. CR 704.3 + CR 704.5a: a player brought to 0 life by the delivery loses
/// before anyone gets priority.
#[test]
fn viper_last_decline_checks_state_based_actions_before_priority() {
    let (mut scenario, viper) = viper_board(2, 1);
    scenario.with_life(P1, 4);
    let mut runner = viper_attacks(scenario, viper);

    decline(&mut runner);
    assert_eq!(
        runner.life(P1),
        4,
        "reach guard: no loss before the last answer"
    );
    decline(&mut runner);

    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    );
    assert!(poll(&runner).is_none());
}

/// V1d. A last answer that is a pick settles through the reducer epilogue's
/// pipeline, once, through either ward pick arm.
#[test]
fn viper_last_pick_runs_one_pipeline_through_either_pick_arm() {
    let Some(db) = shared_card_db() else {
        return;
    };
    for branch in [Branch::Sacrifice, Branch::Discard] {
        let (mut scenario, viper) = viper_board(2, 1);
        let blood = scenario.add_real_card(P0, "Exquisite Blood", Zone::Battlefield, db);
        let mut runner = viper_attacks(scenario, viper);
        let picked = match branch {
            Branch::Sacrifice => object_named(&runner, "Grizzly Bears 1"),
            Branch::Discard => object_named(&runner, "Filler Card 1"),
        };

        decline(&mut runner);
        choose_branch(&mut runner, branch);
        assert!(
            matches!(
                (&runner.state().waiting_for, branch),
                (WaitingFor::WardSacrificeChoice { .. }, Branch::Sacrifice)
                    | (WaitingFor::WardDiscardChoice { .. }, Branch::Discard)
            ),
            "{branch:?}: reach guard: the pick prompt is open, got {:?}",
            runner.state().waiting_for
        );
        assert_eq!(runner.life(P1), 20, "{branch:?}: reach guard");
        pick(&mut runner, picked);

        assert_eq!(
            stack_entries_from(&runner, blood),
            1,
            "{branch:?}: the one declined instance triggers once"
        );
        assert_eq!(runner.life(P1), 16, "{branch:?}");
        assert_eq!(zone_of(&runner, picked), Zone::Graveyard, "{branch:?}");
        runner.advance_until_stack_empty();
        assert_eq!(runner.life(P0), 24, "{branch:?}");
    }
}

/// V1e. CR 704.3 + CR 704.5a: the pick arms check state-based actions too.
#[test]
fn viper_last_pick_checks_state_based_actions_before_priority() {
    for branch in [Branch::Sacrifice, Branch::Discard] {
        let (mut scenario, viper) = viper_board(2, 1);
        scenario.with_life(P1, 4);
        let mut runner = viper_attacks(scenario, viper);
        let picked = match branch {
            Branch::Sacrifice => object_named(&runner, "Grizzly Bears 1"),
            Branch::Discard => object_named(&runner, "Filler Card 1"),
        };

        decline(&mut runner);
        choose_branch(&mut runner, branch);
        pick(&mut runner, picked);

        assert_eq!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(P0) },
            "{branch:?}"
        );
        assert!(poll(&runner).is_none(), "{branch:?}");
    }
}

/// V5. CR 608.2d: an opponent with nothing to sacrifice or discard has only the
/// option not to pay, so it is not prompted at all.
#[test]
fn viper_opponent_with_nothing_to_pay_is_not_prompted() {
    let (scenario, viper) = bare_viper_board(2, 1);
    let runner = viper_attacks(scenario, viper);

    assert!(
        runner.state().stack.is_empty(),
        "reach guard: the attack trigger resolved"
    );
    assert_eq!(blight_counters(&runner, viper), 2);
    assert_eq!(runner.life(P1), 12, "both instances went unpaid");
    assert_poll_released(&runner, P0, "after the first walk");
}

/// V6. CR 608.2d + CR 118.3: once every payable object is chosen, the remaining
/// instances go unpaid without a prompt.
#[test]
fn viper_exhausted_pool_declines_the_remaining_instance() {
    let (mut scenario, viper) = bare_viper_board(2, 1);
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = viper_attacks(scenario, viper);

    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, bears);

    assert_eq!(zone_of(&runner, bears), Zone::Graveyard);
    assert_eq!(runner.life(P1), 16, "the second instance went unpaid");
    assert_poll_released(&runner, P0, "after the only pick");
}

/// The board of V7 / A1: P1 controls two creatures and holds a card; P1 chose
/// its Grizzly Bears for the first instance and is picking a permanent for the
/// second.
fn viper_second_sacrifice_pick() -> (GameRunner, ObjectId, ObjectId) {
    let (mut scenario, viper) = viper_board(2, 1);
    let other = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let mut runner = viper_attacks(scenario, viper);
    let bears = object_named(&runner, "Grizzly Bears 1");

    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, bears);
    choose_branch(&mut runner, Branch::Sacrifice);
    match &runner.state().waiting_for {
        WaitingFor::WardSacrificeChoice {
            player, permanents, ..
        } => {
            assert_eq!(*player, P1);
            assert_eq!(
                permanents,
                &vec![other],
                "reach guard: the second pick excludes the first"
            );
        }
        other => panic!("expected P1's second sacrifice pick, got {other:?}"),
    }
    (runner, bears, other)
}

/// V7. CR 101.4 + CR 118.3: a pick naming the earlier instance's object is
/// refused against the live pool, not the prompt's list, and changes nothing.
#[test]
fn viper_pick_naming_an_earlier_choice_is_refused() {
    let (mut runner, bears, _) = viper_second_sacrifice_pick();
    let before = runner.state().clone();

    let refusal = runner
        .act(GameAction::SelectCards { cards: vec![bears] })
        .expect_err("an earlier instance's pick is refused");

    assert!(
        matches!(refusal, EngineError::InvalidAction(_)),
        "got {refusal:?}"
    );
    assert_eq!(runner.state(), &before, "the refused pick changes nothing");
}

/// A1. The AI's candidates at the second pick come from the prompt's reduced
/// pool, so they never name the earlier pick.
#[test]
fn viper_second_pick_ai_candidates_exclude_the_earlier_pick() {
    let (runner, bears, other) = viper_second_sacrifice_pick();
    let actions = engine::ai_support::legal_actions(runner.state());

    assert!(
        actions.contains(&GameAction::SelectCards { cards: vec![other] }),
        "reach guard: the remaining permanent is a candidate: {actions:?}"
    );
    assert!(
        !actions.contains(&GameAction::SelectCards { cards: vec![bears] }),
        "the earlier pick is not a candidate: {actions:?}"
    );
}

/// V9. CR 616.1 on the delivery's discard: the affected card's owner orders
/// Leyline of the Void and Rest in Peace, then the parked unpaid instances run
/// and the held trigger settles at that answer's resume.
#[test]
fn viper_discard_replacement_choice_resumes_the_rest_of_the_delivery() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(3, 1);
    scenario.add_real_card(P0, "Leyline of the Void", Zone::Battlefield, db);
    scenario.add_real_card(P2, "Rest in Peace", Zone::Battlefield, db);
    let mut runner = viper_attacks(scenario, viper);
    let card = object_named(&runner, "Filler Card 1");

    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card);
    decline(&mut runner);
    decline(&mut runner);
    decline(&mut runner);

    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { player, .. } => assert_eq!(*player, P1),
        other => panic!("expected P1's CR 616.1 choice, got {other:?}"),
    }
    // Reach guard: the discard is delivered first, and the unpaid instances
    // wait behind it.
    assert_eq!(
        delivery(&runner).expect("the delivery is parked").unpaid,
        vec![P1, P2, P2]
    );
    assert_eq!((runner.life(P1), runner.life(P2)), (20, 20));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P1 orders the replacements");

    assert_eq!(zone_of(&runner, card), Zone::Exile);
    assert_eq!(runner.life(P1), 16);
    assert_eq!(runner.life(P2), 12);
    assert_eq!(runner.life(P0), 20);
    assert_poll_released(&runner, P0, "after the replacement answer");
}

/// V9b. CR 101.4 + CR 614.1 + CR 616.1: the discard is delivered first and
/// pauses on P2's choice; once it is answered the unpaid instances follow, and
/// the sacrifice batch comes last with its own choice for P1's Bears.
#[test]
fn viper_discard_then_sacrifice_replacement_choices_resume_in_order() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(3, 1);
    scenario.add_real_card(P0, "Leyline of the Void", Zone::Battlefield, db);
    scenario.add_real_card(P2, "Rest in Peace", Zone::Battlefield, db);
    let mut runner = viper_attacks(scenario, viper);
    let bears = object_named(&runner, "Grizzly Bears 1");
    let card = object_named(&runner, "Filler Card 2");

    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, bears);
    decline(&mut runner);
    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card);
    decline(&mut runner);

    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { player, .. } => assert_eq!(*player, P2),
        other => panic!("expected P2's CR 616.1 choice for its card, got {other:?}"),
    }
    assert_eq!(
        zone_of(&runner, bears),
        Zone::Battlefield,
        "reach guard: the sacrifice batch is delivered last"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P2 orders the replacements");
    assert_eq!(zone_of(&runner, card), Zone::Exile);
    assert_eq!(
        (runner.life(P1), runner.life(P2)),
        (16, 16),
        "the unpaid instances were delivered after the discard"
    );
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { player, .. } => assert_eq!(*player, P1),
        other => panic!("expected P1's CR 616.1 choice for the Bears, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P1 orders the replacements");
    assert_eq!(zone_of(&runner, bears), Zone::Exile);

    assert_eq!(runner.life(P1), 16);
    assert_eq!(runner.life(P2), 16);
    assert_eq!(runner.life(P0), 20);
    assert_poll_released(&runner, P0, "after the second replacement answer");
}

/// V9c. CR 101.4 + CR 603.10 + CR 616.1: P1's discard pauses on its replacement
/// choice, and P2's sacrifice is delivered last, so at the pause the Bears are
/// still on the battlefield and Mazirek, Kraul Death Priest ("Whenever a player
/// sacrifices another permanent, put a +1/+1 counter on each creature you
/// control.") has not triggered. After the answer the Bears are sacrificed and
/// the trigger goes on the stack once a player would receive priority.
#[test]
fn viper_last_sacrifice_pick_is_delivered_after_the_paused_discard_and_keeps_its_triggers() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(3, 0);
    let mazirek = scenario.add_real_card(P0, "Mazirek, Kraul Death Priest", Zone::Battlefield, db);
    // P2's Leyline replaces only an opponent's card, so P2's Bears meet Rest in
    // Peace alone while P1's card meets both.
    scenario.add_real_card(P2, "Leyline of the Void", Zone::Battlefield, db);
    scenario.add_real_card(P2, "Rest in Peace", Zone::Battlefield, db);
    let mut runner = viper_attacks(scenario, viper);
    let card = object_named(&runner, "Filler Card 1");
    let bears = object_named(&runner, "Grizzly Bears 2");

    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card);
    choose_branch(&mut runner, Branch::Sacrifice);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::WardSacrificeChoice { player: P2, .. }
        ),
        "reach guard: the last answer is P2's sacrifice pick, got {:?}",
        runner.state().waiting_for
    );
    pick(&mut runner, bears);

    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { player, .. } => assert_eq!(*player, P1),
        other => panic!("expected P1's CR 616.1 choice for its card, got {other:?}"),
    }
    assert_eq!(
        zone_of(&runner, bears),
        Zone::Battlefield,
        "reach guard: the sacrifice is delivered after the discard"
    );
    assert_eq!(triggers_from(&runner, mazirek), 0);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P1 orders the replacements");

    assert_eq!(zone_of(&runner, card), Zone::Exile);
    assert_eq!(zone_of(&runner, bears), Zone::Exile);
    assert_eq!(runner.life(P1), 20);
    assert_eq!(runner.life(P2), 20);
    assert_poll_released(&runner, P0, "after the replacement answer");
    assert_eq!(
        stack_entries_from(&runner, mazirek) + deferred_from(&runner, mazirek),
        1,
        "Mazirek's trigger on P2's sacrifice is put on the stack"
    );

    runner.advance_until_stack_empty();
    for creature in [viper, mazirek] {
        assert_eq!(
            runner.state().objects[&creature]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(1),
            "Mazirek's trigger resolved: a +1/+1 counter on each creature P0 controls"
        );
    }
}

/// The Megrim board for the V9d rows: Rottenmouth Viper and Megrim
/// ("Whenever an opponent discards a card, this enchantment deals 2 damage to
/// that player.") under P0, Leyline of the Void under P1 and Rest in Peace
/// under P2. P1's own Leyline does not replace P1's card, so P1's discard meets
/// Rest in Peace alone and completes; P2's card meets both, so P2's discard
/// pauses on P2's CR 616.1 choice after P1's has completed in the same discard
/// node.
fn viper_megrim_board(
    player_count: u8,
    starting_blight_counters: u32,
) -> Option<(GameRunner, ObjectId)> {
    let db = shared_card_db()?;
    let (mut scenario, viper) = viper_board(player_count, starting_blight_counters);
    let megrim = scenario.add_real_card(P0, "Megrim", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Leyline of the Void", Zone::Battlefield, db);
    scenario.add_real_card(P2, "Rest in Peace", Zone::Battlefield, db);
    Some((viper_attacks(scenario, viper), megrim))
}

/// Triggers of `source` queued, awaiting their controller's CR 603.3b ordering,
/// or on the stack.
fn triggers_from(runner: &GameRunner, source: ObjectId) -> usize {
    let ordering = match &runner.state().waiting_for {
        WaitingFor::OrderTriggers { triggers, .. } => triggers
            .iter()
            .filter(|trigger| trigger.source_id == source)
            .count(),
        _ => 0,
    };
    stack_entries_from(runner, source) + deferred_from(runner, source) + ordering
}

/// The V9d pause: P1's discard completed (its card is in exile) and P2's
/// discard waits on P2's CR 616.1 choice. CR 603.10: the delivery is one
/// simultaneous event, so P1's completed discard is held until the delivery
/// completes and has triggered nothing yet.
fn assert_paused_after_one_completed_discard(
    runner: &GameRunner,
    megrim: ObjectId,
    completed: ObjectId,
    paused: ObjectId,
    context: &str,
) {
    assert_eq!(
        zone_of(runner, completed),
        Zone::Exile,
        "{context}: reach guard: P1's discard completed before the pause"
    );
    assert_eq!(
        zone_of(runner, paused),
        Zone::Hand,
        "{context}: reach guard: P2's card is still in hand"
    );
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice { player, .. } => {
            assert_eq!(*player, P2, "{context}: P2 orders the replacements")
        }
        other => panic!("{context}: expected P2's CR 616.1 choice, got {other:?}"),
    }
    assert_eq!(
        triggers_from(runner, megrim),
        0,
        "{context}: P1's completed discard is held until the delivery completes"
    );
}

/// The V9d resume: P2 orders the replacements, its card is exiled, and each
/// opponent's one discard has triggered Megrim exactly once (CR 603.2).
fn answer_the_paused_discard(runner: &mut GameRunner, megrim: ObjectId, paused: ObjectId) {
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P2 orders the replacements");
    assert_eq!(zone_of(runner, paused), Zone::Exile);
    assert_eq!(
        triggers_from(runner, megrim),
        2,
        "one Megrim trigger for each opponent's discard, got wait {:?}",
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();
    assert_eq!(triggers_from(runner, megrim), 0);
}

/// V9d(a). CR 603.2 + CR 603.2c + CR 616.1: the last answer is P2's discard
/// pick. In the one discard node P1's card is discarded and P2's then pauses;
/// the pause queues Megrim's trigger for P1's discard, and the pick's settle
/// does not collect that discard a second time.
#[test]
fn viper_last_discard_pick_before_a_paused_discard_triggers_megrim_once() {
    let Some((mut runner, megrim)) = viper_megrim_board(3, 0) else {
        return;
    };
    let p1_card = object_named(&runner, "Filler Card 1");
    let p2_card = object_named(&runner, "Filler Card 2");

    assert_eq!(prompted(&runner), P1);
    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, p1_card);
    assert_eq!(prompted(&runner), P2);
    choose_branch(&mut runner, Branch::Discard);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::WardDiscardChoice { player: P2, .. }
        ),
        "reach guard: the last answer is P2's discard pick, got {:?}",
        runner.state().waiting_for
    );
    pick(&mut runner, p2_card);

    assert_paused_after_one_completed_discard(&runner, megrim, p1_card, p2_card, "the pick");
    answer_the_paused_discard(&mut runner, megrim, p2_card);
    assert_eq!(runner.life(P1), 18);
    assert_eq!(runner.life(P2), 18);
    assert_eq!(runner.life(P0), 20);
}

/// V9d(b). As V9d(a) with two blight counters: each opponent discards for one
/// instance and declines the other, so the last answer is P2's decline and
/// the `RunFrom` settle scans the delivery. Each opponent loses 4 life once and
/// takes 2 damage from Megrim once.
#[test]
fn viper_last_decline_before_a_paused_discard_triggers_megrim_once() {
    let Some((mut runner, megrim)) = viper_megrim_board(3, 1) else {
        return;
    };
    let p1_card = object_named(&runner, "Filler Card 1");
    let p2_card = object_named(&runner, "Filler Card 2");

    for (seat, card) in [(P1, p1_card), (P2, p2_card)] {
        assert_eq!(prompted(&runner), seat);
        choose_branch(&mut runner, Branch::Discard);
        pick(&mut runner, card);
        assert_eq!(prompted(&runner), seat);
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::UnlessPayment { .. } | WaitingFor::UnlessPaymentChooseCost { .. }
            ),
            "reach guard: {seat:?}'s second instance is an unless prompt, got {:?}",
            runner.state().waiting_for
        );
        decline(&mut runner);
    }

    assert_paused_after_one_completed_discard(&runner, megrim, p1_card, p2_card, "the decline");
    assert_eq!(
        (runner.life(P1), runner.life(P2)),
        (20, 20),
        "the unpaid life losses follow the discards"
    );
    answer_the_paused_discard(&mut runner, megrim, p2_card);
    assert_eq!(runner.life(P1), 14);
    assert_eq!(runner.life(P2), 14);
    assert_eq!(runner.life(P0), 20);
}

/// V9d(c). CR 104.3a + CR 800.4f + CR 603.2c: the last chooser concedes, which
/// completes collection, and the concession delivers the poll. P1's discard
/// completes and P2's pauses inside that delivery; the departure's collection
/// of the delivery does not collect P1's discard a second time.
#[test]
fn viper_last_chooser_conceding_before_a_paused_discard_triggers_megrim_once() {
    let Some((mut runner, megrim)) = viper_megrim_board(4, 0) else {
        return;
    };
    let p1_card = object_named(&runner, "Filler Card 1");
    let p2_card = object_named(&runner, "Filler Card 2");

    for (seat, card) in [(P1, p1_card), (P2, p2_card)] {
        assert_eq!(prompted(&runner), seat);
        choose_branch(&mut runner, Branch::Discard);
        pick(&mut runner, card);
    }
    assert_eq!(
        poll(&runner)
            .and_then(|carrier| carrier.collection())
            .map(|collection| collection.current_player),
        Some(P3),
        "reach guard: P3 is the last chooser"
    );
    assert_eq!(zone_of(&runner, p1_card), Zone::Hand);

    concede(&mut runner, P3);

    assert!(is_eliminated(&runner, P3));
    assert!(
        delivery(&runner).is_some(),
        "the concession delivered the poll up to the paused step"
    );
    assert_paused_after_one_completed_discard(&runner, megrim, p1_card, p2_card, "the concession");
    answer_the_paused_discard(&mut runner, megrim, p2_card);
    assert_eq!(runner.life(P1), 18);
    assert_eq!(runner.life(P2), 18);
    assert_eq!(runner.life(P0), 20);
}

/// V10. CR 118.12: the chosen discard pays a cost, so Library of Leng ("If an
/// effect causes you to discard a card, …") does not apply.
#[test]
fn viper_discard_is_a_cost_library_of_leng_does_not_apply() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(2, 0);
    scenario.add_real_card(P1, "Library of Leng", Zone::Battlefield, db);
    let mut runner = viper_attacks(scenario, viper);
    let card = object_named(&runner, "Filler Card 1");

    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card);

    assert_eq!(zone_of(&runner, card), Zone::Graveyard);
    assert_eq!(runner.life(P1), 20);
    assert_poll_released(&runner, P0, "no top-of-library option was offered");
}

/// V10b. CR 702.35a: a madness card chosen for the discard is discarded into
/// exile and its madness trigger follows.
#[test]
fn viper_madness_card_chosen_for_the_discard_is_exiled() {
    let (mut scenario, viper) = bare_viper_board(2, 0);
    let temper = scenario
        .add_spell_to_hand_from_oracle(P1, "Fiery Temper", true, FIERY_TEMPER)
        .id();
    let mut runner = viper_attacks(scenario, viper);

    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, temper);

    assert_eq!(zone_of(&runner, temper), Zone::Exile);
    assert_eq!(runner.life(P1), 20);
    assert_eq!(
        stack_entries_from(&runner, temper) + deferred_from(&runner, temper),
        1,
        "the madness trigger is offered: {:?}",
        runner.state().waiting_for
    );
}

/// V11. CR 608.2h + the ruling ("use the number of blight counters that were on
/// it as it last existed on the battlefield"): sacrificed in response, the
/// Viper gets no counter from its own trigger and the count is fixed once.
#[test]
fn viper_sacrificed_in_response_counts_its_last_known_counters() {
    let (mut scenario, viper) = viper_board(2, 1);
    let altar = scenario
        .add_artifact_from_oracle(P0, "Ashnod's Altar", ASHNODS_ALTAR)
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(viper, AttackTarget::Player(P1))])
        .expect("Viper attacks P1");
    assert_eq!(
        stack_entries_from(&runner, viper),
        1,
        "reach guard: the attack trigger is on the stack"
    );

    runner
        .act(activate_altar(altar))
        .expect("P0 activates Ashnod's Altar");
    if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        pick(&mut runner, viper);
    }
    assert_eq!(
        zone_of(&runner, viper),
        Zone::Graveyard,
        "the Viper was sacrificed"
    );
    pass_until_prompt(&mut runner);

    let carrier = poll(&runner).expect("the poll is open");
    assert_eq!(carrier.instances, 1, "one counter as it last existed");
    decline(&mut runner);
    assert_eq!(runner.life(P1), 16);
}

/// V12. CR 104.3a + CR 800.4f + CR 704.3: the last chooser concedes, which
/// completes collection. The concession's own action delivers the poll, puts
/// its life-loss trigger on the stack once and checks state-based actions
/// before anyone gets priority.
#[test]
fn viper_last_chooser_conceding_delivers_the_poll_in_the_concession() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(4, 0);
    let blood = scenario.add_real_card(P0, "Exquisite Blood", Zone::Battlefield, db);
    scenario.with_life(P1, 4);
    let mut runner = viper_attacks(scenario, viper);
    let bears = object_named(&runner, "Grizzly Bears 2");

    decline(&mut runner);
    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, bears);
    assert_eq!(
        poll(&runner)
            .and_then(|carrier| carrier.collection())
            .map(|collection| collection.current_player),
        Some(P3)
    );
    assert_eq!(zone_of(&runner, bears), Zone::Battlefield);
    assert_eq!(runner.life(P1), 4);
    assert_poll_holds_its_resolution(&runner, viper, "P3's prompt");

    concede(&mut runner, P3);

    assert_eq!(zone_of(&runner, bears), Zone::Graveyard);
    assert!(
        is_eliminated(&runner, P1),
        "CR 704.5a: P1 lost before anyone got priority"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        stack_entries_from(&runner, blood),
        1,
        "P1's life loss triggers Exquisite Blood once"
    );
    assert_eq!(stack_entries_from(&runner, viper), 0);
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(poll(&runner).is_none());
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), 24);
}

/// V12b. CR 800.4a: the Viper's controller concedes mid-collection; its
/// resolving trigger ceases to exist, nothing is delivered, and priority goes
/// to the next player in turn order still in the game (CR 117.3b + CR 800.4j).
#[test]
fn viper_controller_conceding_abandons_the_poll() {
    let (mut runner, viper) = viper_attacking_with_blight_counters(3, 1);
    let bears = object_named(&runner, "Grizzly Bears 1");

    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, bears);
    assert_eq!(
        poll(&runner)
            .and_then(|carrier| carrier.collection())
            .map(|collection| collection.current_player),
        Some(P1)
    );
    assert_eq!(zone_of(&runner, bears), Zone::Battlefield);
    assert_poll_holds_its_resolution(&runner, viper, "P1's second prompt");

    concede(&mut runner, P0);

    assert!(poll(&runner).is_none());
    assert_eq!(
        zone_of(&runner, bears),
        Zone::Battlefield,
        "nothing delivered"
    );
    assert_eq!(runner.life(P1), 20);
    assert_eq!(runner.life(P2), 20);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    );
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(runner.state().resolution_stack.is_empty());
    assert_eq!(stack_entries_from(&runner, viper), 0);

    let restored = round_trip(runner.state());
    assert!(restored.pending_simultaneous_unless.is_none());
    assert!(restored.resolving_stack_entry.is_none());
}

/// V12c. CR 800.4a + CR 118.12: a chosen permanent the leaver owns leaves the
/// game with it; the pick is voided, nothing is sacrificed, and its instance
/// still counts as paid.
#[test]
fn viper_pick_voided_by_a_departure_costs_no_life() {
    let (mut scenario, viper) = viper_board(3, 0);
    let control = scenario
        .add_enchantment_from_oracle(P1, "Mind Control", MIND_CONTROL)
        .with_subtypes(vec!["Aura"])
        .id();
    let mut runner = scenario.build();
    let stolen = object_named(&runner, "Grizzly Bears 2");
    {
        let state = runner.state_mut();
        state.objects.get_mut(&control).unwrap().attached_to = Some(stolen.into());
        state
            .objects
            .get_mut(&stolen)
            .unwrap()
            .attachments
            .push(control);
        state.layers_dirty.mark_full();
    }
    engine::game::layers::flush_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&stolen].controller,
        P1,
        "reach guard: P1 controls P2's Bears"
    );
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(viper, AttackTarget::Player(P1))])
        .expect("Viper attacks P1");
    runner.advance_until_stack_empty();

    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, stolen);
    assert_eq!(prompted(&runner), P2, "reach guard: P2 is choosing");
    assert_poll_holds_its_resolution(&runner, viper, "P2's prompt");

    concede(&mut runner, P2);

    assert_ne!(
        zone_of(&runner, stolen),
        Zone::Graveyard,
        "the pick left the game with its owner and was not sacrificed"
    );
    assert_eq!(
        runner.life(P1),
        20,
        "the voided pick's instance costs no life"
    );
    assert_poll_released(&runner, P0, "after the concession");
}

/// V12d. CR 800.4f: a seat that leaves after answering is pruned with its
/// answers and its hidden pick; the current chooser's prompt stands.
#[test]
fn viper_answered_seat_conceding_is_pruned() {
    let (mut runner, viper) = viper_attacking_with_blight_counters(3, 1);
    let card = object_named(&runner, "Filler Card 1");

    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card);
    decline(&mut runner);
    assert_eq!(prompted(&runner), P2, "reach guard: P2 is choosing");

    concede(&mut runner, P1);

    assert_eq!(prompted(&runner), P2, "P2's prompt stands");
    assert_poll_holds_its_resolution(&runner, viper, "after P1 left");
    let carrier = poll(&runner).unwrap();
    assert!(carrier
        .collection()
        .unwrap()
        .answers
        .iter()
        .all(|answer| answer.player != P1));
    assert!(carrier.collection().unwrap().hidden_discards.is_empty());
    decline(&mut runner);
    decline(&mut runner);

    assert_eq!(runner.life(P2), 12);
    assert_eq!(runner.life(P0), 20);
    assert_ne!(
        zone_of(&runner, card),
        Zone::Graveyard,
        "the card left with P1"
    );
    assert_poll_released(&runner, P0, "after P2's last decline");
}

/// V12e. CR 704.3 + CR 704.5f: the delivery a concession completes is followed
/// by a full state-based-action pass: Maro ("Maro's power and toughness are each
/// equal to the number of cards in your hand.") dies with its controller's last
/// card discarded, before anyone gets priority.
#[test]
fn viper_concession_delivery_checks_every_state_based_action() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(3, 0);
    let maro = scenario.add_real_card(P1, "Maro", Zone::Battlefield, db);
    let mut runner = viper_attacks(scenario, viper);
    let card = object_named(&runner, "Filler Card 1");

    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card);
    assert_eq!(
        poll(&runner)
            .and_then(|carrier| carrier.collection())
            .map(|collection| collection.current_player),
        Some(P2)
    );
    assert_eq!(zone_of(&runner, maro), Zone::Battlefield);
    assert_eq!(zone_of(&runner, card), Zone::Hand);
    assert_poll_holds_its_resolution(&runner, viper, "P2's prompt");

    concede(&mut runner, P2);

    assert_eq!(zone_of(&runner, card), Zone::Graveyard);
    assert_eq!(zone_of(&runner, maro), Zone::Graveyard);
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(poll(&runner).is_none());
}

/// V12f. CR 800.4a + CR 704.5a: a concession removes Abyssal Persecutor ("You
/// can't win the game and your opponents can't lose the game."); the action
/// boundary's player-loss check then eliminates the 0-life chooser, whose
/// departure completes collection and settles through that check.
#[test]
fn viper_player_loss_check_completing_collection_settles_the_poll() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(4, 0);
    let blood = scenario.add_real_card(P0, "Exquisite Blood", Zone::Battlefield, db);
    let persecutor = scenario
        .add_creature_from_oracle(P1, "Abyssal Persecutor", 6, 6, ABYSSAL_PERSECUTOR)
        .id();
    scenario.with_life(P3, 0);
    let mut runner = viper_attacks(scenario, viper);

    decline(&mut runner);
    decline(&mut runner);
    assert_eq!(
        poll(&runner)
            .and_then(|carrier| carrier.collection())
            .map(|collection| collection.current_player),
        Some(P3)
    );
    assert_eq!(runner.life(P3), 0);
    assert!(!is_eliminated(&runner, P3));
    assert_eq!(zone_of(&runner, persecutor), Zone::Battlefield);
    assert_poll_holds_its_resolution(&runner, viper, "P3's prompt");

    concede(&mut runner, P1);

    assert_eq!(runner.life(P1), 20, "P1 pays nothing");
    assert_eq!(runner.life(P2), 16);
    assert!(is_eliminated(&runner, P1));
    assert!(is_eliminated(&runner, P3));
    assert_eq!(
        stack_entries_from(&runner, blood),
        1,
        "P2's loss triggers once"
    );
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(poll(&runner).is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P0), 24);
}

/// V12g. CR 117.3b + CR 800.4j: on another player's turn, the departed
/// controller's abandoned poll hands priority to the active player.
#[test]
fn viper_controller_conceding_on_another_turn_gives_priority_to_the_active_player() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(3, 0);
    let ephemerate = scenario.add_real_card(P0, "Ephemerate", Zone::Hand, db);
    scenario.add_basic_land(P0, ManaColor::White);
    // Every seat draws through the turns passed below.
    for seat in 0..3 {
        scenario.with_library_top(PlayerId(seat), &["Library A", "Library B", "Library C"]);
    }
    let mut runner = scenario.build();
    // Real turn structure: pass priority (declaring no attackers on P0's own
    // turn) until P2 is the active player and P0 holds priority on it.
    for _ in 0..64 {
        let state = runner.state();
        if state.active_player == P2
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
        {
            break;
        }
        match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("no attackers");
            }
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("the pass is accepted");
            }
        }
    }
    assert_eq!(runner.state().active_player, P2, "P2's turn was reached");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "P0 holds priority on P2's turn: {:?}",
        runner.state().waiting_for
    );

    let outcome = runner.cast(ephemerate).target_object(viper).resolve();
    drop(outcome);

    // CR 400.7 + CR 122.2: the returned Viper is a new object with no counters,
    // so its enters trigger leaves it with one.
    let carrier = poll(&runner).expect("the enters trigger opened the poll");
    assert_eq!(carrier.instances, 1);
    assert_eq!(
        carrier.collection().unwrap().current_player,
        P2,
        "APNAP from the active player"
    );
    assert!(runner.state().resolving_stack_entry.is_some());

    concede(&mut runner, P0);

    assert!(poll(&runner).is_none());
    assert!(runner.state().resolving_stack_entry.is_none());
    assert_eq!(runner.life(P1), 20);
    assert_eq!(runner.life(P2), 20);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P2 }
    );
}

/// V12h board: Doubling Season under P0 and Vorinclex, Monstrous Raider under
/// P1, so the trigger's own blight counter pauses on a CR 616.1 order (×2 and
/// half don't commute). P0 applies Doubling Season first: 1 → 2 → 1.
fn viper_counter_placement_paused(player_count: u8) -> (GameRunner, ObjectId) {
    let db = shared_card_db().expect("the integration fixture is present");
    let (mut scenario, viper) = viper_board(player_count, 1);
    scenario.add_real_card(P0, "Doubling Season", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Vorinclex, Monstrous Raider", Zone::Battlefield, db);
    let mut runner = viper_attacks_without_advancing(scenario, viper);

    let index = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player,
            candidate_count,
            candidates,
            ..
        } => {
            assert_eq!(*player, P0, "CR 616.1: the affected object's controller");
            assert_eq!(*candidate_count, 2);
            let mut names: Vec<&str> = candidates
                .iter()
                .map(|candidate| candidate.source_name.as_str())
                .collect();
            names.sort_unstable();
            assert_eq!(names, ["Doubling Season", "Vorinclex, Monstrous Raider"]);
            candidates
                .iter()
                .position(|candidate| candidate.source_name == "Doubling Season")
                .unwrap()
        }
        other => panic!("expected the CR 616.1 choice, got {other:?}"),
    };
    assert_eq!(
        blight_counters(&runner, viper),
        1,
        "the placement is paused"
    );
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("P0 applies Doubling Season first");

    assert_eq!(blight_counters(&runner, viper), 2, "CR 616.1f: 1 → 2 → 1");
    let carrier = poll(&runner).expect("the poll is open");
    assert_eq!(carrier.instances, 2);
    assert_eq!(carrier.collection().unwrap().current_player, P1);
    assert!(
        runner.state().resolution_stack.is_empty(),
        "the counter frame retired and the continuation was taken"
    );
    assert_poll_holds_its_resolution(&runner, viper, "after the CR 616.1 answer");
    (runner, viper)
}

/// V12h(a). The answer route after the trigger's own placement paused.
#[test]
fn viper_poll_after_a_paused_counter_placement_settles_on_the_last_answer() {
    if shared_card_db().is_none() {
        return;
    }
    let (mut runner, _) = viper_counter_placement_paused(2);
    decline(&mut runner);
    decline(&mut runner);

    assert_eq!(runner.life(P1), 12);
    assert_eq!(runner.life(P0), 20);
    assert!(runner.state().resolution_stack.is_empty());
    assert_poll_released(&runner, P0, "after the last decline");
}

/// V12h(b). The controller-leaves route after the same pause.
#[test]
fn viper_poll_after_a_paused_counter_placement_is_abandoned_by_its_controller() {
    if shared_card_db().is_none() {
        return;
    }
    let (mut runner, _) = viper_counter_placement_paused(3);
    concede(&mut runner, P0);

    assert!(poll(&runner).is_none());
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(runner.state().resolution_stack.is_empty());
    assert_eq!(runner.life(P1), 20);
    assert_eq!(runner.life(P2), 20);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    );
    round_trip(runner.state());
}

/// V12i board: P2's Grizzly Bears is controlled by P1 (the board Ray of Command
/// leaves after resolving during P0's turn), and P1 has chosen to sacrifice
/// and is picking. With `with_own_bears`, P1 also controls its own Bears.
fn viper_pick_over_a_borrowed_creature(
    with_own_bears: bool,
) -> (GameRunner, ObjectId, ObjectId, Option<ObjectId>) {
    let (mut scenario, viper) = bare_viper_board(3, 0);
    let borrowed = scenario
        .add_creature(P2, "Borrowed Bears", 2, 2)
        .controlled_by(P1)
        .id();
    let own = with_own_bears.then(|| scenario.add_creature(P1, "Own Bears", 2, 2).id());
    let mut runner = viper_attacks(scenario, viper);

    choose_branch(&mut runner, Branch::Sacrifice);
    match &runner.state().waiting_for {
        WaitingFor::WardSacrificeChoice {
            player, permanents, ..
        } => {
            assert_eq!(*player, P1);
            let mut expected: Vec<ObjectId> = own.into_iter().chain([borrowed]).collect();
            let mut offered = permanents.clone();
            expected.sort();
            offered.sort();
            assert_eq!(
                offered, expected,
                "reach guard: the pick lists the borrowed Bears"
            );
        }
        other => panic!("expected P1's sacrifice pick, got {other:?}"),
    }
    assert_poll_holds_its_resolution(&runner, viper, "P1's pick");
    (runner, viper, borrowed, own)
}

/// V12i(a). CR 800.4a + CR 118.3 + CR 608.2d: the departure removes the only
/// object of an open pick; the refreshed prompt has nothing to offer, so the
/// instance goes unpaid and collection completes: the poll releases its
/// resolution.
#[test]
fn viper_departure_emptying_an_open_pick_releases_the_poll() {
    let (mut runner, _, borrowed, _) = viper_pick_over_a_borrowed_creature(false);

    concede(&mut runner, P2);

    assert_ne!(
        zone_of(&runner, borrowed),
        Zone::Battlefield,
        "it left with P2"
    );
    assert_eq!(runner.life(P1), 16, "the instance went unpaid");
    assert_poll_released(&runner, P0, "within the concession");
}

/// V12i(b). The departure removes one object of an open pick; the pick is
/// republished with what remains.
#[test]
fn viper_departure_shrinking_an_open_pick_republishes_it() {
    let (mut runner, _, borrowed, own) = viper_pick_over_a_borrowed_creature(true);
    let own = own.unwrap();

    concede(&mut runner, P2);

    match &runner.state().waiting_for {
        WaitingFor::WardSacrificeChoice {
            player, permanents, ..
        } => {
            assert_eq!(*player, P1);
            assert_eq!(permanents, &vec![own], "the pick lists only what remains");
        }
        other => panic!("expected P1's republished pick, got {other:?}"),
    }
    assert!(runner
        .act(GameAction::SelectCards {
            cards: vec![borrowed]
        })
        .is_err());
    pick(&mut runner, own);

    assert_eq!(zone_of(&runner, own), Zone::Graveyard);
    assert_eq!(runner.life(P1), 20);
    assert_poll_released(&runner, P0, "after the pick");
}

/// V14. CR 605.3a + CR 608.2g: a permanent chosen for an earlier instance can't
/// be consumed by a mana ability at a later prompt; Ashnod's Altar is refused
/// at both the instance-2 unless prompt and its pick.
#[test]
fn viper_chosen_permanent_cannot_be_consumed_by_a_mana_ability() {
    let (mut scenario, viper) = bare_viper_board(2, 1);
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let altar = scenario
        .add_artifact_from_oracle(P1, "Ashnod's Altar", ASHNODS_ALTAR)
        .id();
    let swamp = scenario.add_basic_land(P1, ManaColor::Black);
    let mut runner = viper_attacks(scenario, viper);

    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, bears);
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::UnlessPayment {
                player: P1,
                cost: AbilityCost::Sacrifice(_),
                ..
            }
        ),
        "reach guard: instance 2 is a single-branch sacrifice: {:?}",
        runner.state().waiting_for
    );
    for stage in ["the unless prompt", "the pick"] {
        assert!(
            offered_mana_actions(&runner).is_empty(),
            "{stage}: no mana action is offered"
        );
        let refusals = submit_refused_mana_actions(
            &mut runner,
            P1,
            swamp,
            &[bears, giant, altar],
            vec![activate_altar(altar)],
        );
        if stage == "the unless prompt" {
            assert!(
                refusals.iter().all(names_the_manaless_payment),
                "CR 605.3a refuses the mana action: {refusals:?}"
            );
            choose_branch(&mut runner, Branch::Sacrifice);
        }
    }
    pick(&mut runner, giant);

    assert_eq!(zone_of(&runner, bears), Zone::Graveyard);
    assert_eq!(zone_of(&runner, giant), Zone::Graveyard);
    assert_eq!(runner.life(P1), 20);
}

/// V18 / V18b board: Stalwart Successor ("Whenever one or more counters are put
/// on a creature you control, if it's the first time counters have been put on
/// that creature this turn, put a +1/+1 counter on that creature.") under P0
/// triggers on the Viper's blight counter.
fn viper_with_stalwart_successors(count: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let db = shared_card_db().expect("the integration fixture is present");
    let (mut scenario, viper) = viper_board(2, 0);
    let stalwarts: Vec<ObjectId> = (0..count)
        .map(|_| scenario.add_real_card(P0, "Stalwart Successor", Zone::Battlefield, db))
        .collect();
    let runner = viper_attacks_without_advancing(scenario, viper);
    (runner, viper, stalwarts)
}

fn stalwart_triggers(runner: &GameRunner, stalwarts: &[ObjectId]) -> usize {
    stalwarts
        .iter()
        .map(|stalwart| stack_entries_from(runner, *stalwart) + deferred_from(runner, *stalwart))
        .sum()
}

/// V18. CR 603.3 + CR 117.2e: what the trigger's blight counter triggers waits
/// in the queue while the poll collects, and is put on the stack the next time
/// a player would receive priority.
#[test]
fn viper_counter_trigger_waits_for_the_poll() {
    if shared_card_db().is_none() {
        return;
    }
    let (mut runner, viper, stalwarts) = viper_with_stalwart_successors(1);
    let stalwart = stalwarts[0];

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::UnlessPaymentChooseCost { player: P1, .. }
    ));
    assert_eq!(blight_counters(&runner, viper), 1);
    assert_eq!(
        stalwart_triggers(&runner, &stalwarts),
        1,
        "reach guard: Stalwart Successor triggered on the blight counter"
    );
    assert_poll_holds_its_resolution(&runner, viper, "P1's prompt");
    assert_eq!(
        deferred_from(&runner, stalwart),
        1,
        "the trigger waits in the queue"
    );
    assert_eq!(
        runner.state().objects[&viper]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0
    );

    decline(&mut runner);

    assert_eq!(runner.life(P1), 16);
    assert!(poll(&runner).is_none());
    assert!(runner.state().resolving_stack_entry.is_none());
    assert_eq!(
        stack_entries_from(&runner, stalwart),
        1,
        "placed by the answer's pipeline"
    );
}

/// V18b. Two of the controller's triggers raise no ordering prompt over the
/// poll's prompt.
#[test]
fn viper_counter_triggers_raise_no_ordering_prompt_over_the_poll() {
    if shared_card_db().is_none() {
        return;
    }
    let (mut runner, viper, stalwarts) = viper_with_stalwart_successors(2);

    assert_eq!(stalwart_triggers(&runner, &stalwarts), 2, "reach guard");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPaymentChooseCost { player: P1, .. }
        ) && runner.state().stack.is_empty(),
        "P1's prompt stands over an empty stack: {:?}",
        runner.state().waiting_for
    );
    assert_poll_holds_its_resolution(&runner, viper, "P1's prompt");

    decline(&mut runner);

    assert_eq!(runner.life(P1), 16);
    assert!(poll(&runner).is_none());
    assert!(runner.state().resolving_stack_entry.is_none());
    for stalwart in &stalwarts {
        assert_eq!(
            triggers_from(&runner, *stalwart) - deferred_from(&runner, *stalwart),
            1,
            "each Stalwart Successor trigger left the queue onto the stack or the ordering prompt"
        );
        assert_eq!(deferred_from(&runner, *stalwart), 0);
    }
}

/// V19. Regression: a generic unless prompt (Hag of Ceaseless Torment, not a
/// choose-all-then-act poll) settles its trigger as before, in the action that
/// raises the prompt.
#[test]
fn hag_unless_prompt_settles_its_trigger_before_the_answer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let hag = scenario
        .add_creature_from_oracle(
            P0,
            "Hag of Ceaseless Torment",
            3,
            3,
            HAG_OF_CEASELESS_TORMENT,
        )
        .id();
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    scenario.with_cards_in_hand(P1, &["Filler Card"]);
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    match &runner.state().waiting_for {
        WaitingFor::UnlessPaymentChooseCost {
            player,
            pending_effect,
            ..
        } => {
            assert_eq!(*player, P1);
            assert_eq!(pending_effect.source_id, hag, "reach guard: Hag's prompt");
        }
        other => panic!("expected the opponent's unless prompt, got {other:?}"),
    }
    assert!(poll(&runner).is_none(), "Hag's clause is not a poll");
    assert!(
        runner.state().resolving_stack_entry.is_none(),
        "the trigger was retired in the action that raised the prompt"
    );

    decline(&mut runner);
    assert_eq!(runner.life(P1), 17);
    assert_eq!(runner.life(P0), 20);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

/// P. CR 101.4a + CR 101.4b: earlier answers are public, but a card chosen to
/// be discarded stays unrevealed except to its chooser (Rottenmouth Viper's
/// 2024-07-26 ruling: "cards chosen to be discarded this way won't be revealed
/// until they're discarded").
#[test]
fn viper_hidden_discard_is_projected_only_to_its_chooser() {
    let (mut runner, _) = viper_attacking_with_blight_counters(3, 1);
    let bears = object_named(&runner, "Grizzly Bears 1");
    let card = object_named(&runner, "Filler Card 1");

    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card);
    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, bears);
    assert_eq!(prompted(&runner), P2);
    let carrier = poll(&runner).unwrap();
    assert!(
        carrier
            .collection()
            .unwrap()
            .hidden_discards
            .iter()
            .any(|(seat, pin)| *seat == P1 && pin.object_id == card),
        "reach guard: the unfiltered poll holds P1's hidden pick"
    );

    let p2_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P2);
    let p2_poll = p2_view.pending_simultaneous_unless.as_deref().unwrap();
    assert!(p2_poll.collection().unwrap().hidden_discards.is_empty());
    // CR 101.4a: a viewer projection mid-poll keeps fewer hidden picks than
    // `Discard` answers, and the carrier's validating decode still admits it.
    serde_json::from_value::<PendingSimultaneousUnless>(serde_json::to_value(p2_poll).unwrap())
        .expect("a viewer projection of the poll mid-collection decodes");
    assert!(matches!(
        p2_poll
            .collection()
            .unwrap()
            .answers
            .iter()
            .map(|answer| &answer.answer)
            .collect::<Vec<_>>()
            .as_slice(),
        [
            SimultaneousUnlessAnswer::Discard,
            SimultaneousUnlessAnswer::Sacrifice { permanent }
        ] if permanent.object_id == bears
    ));

    let p1_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P1);
    assert!(p1_view
        .pending_simultaneous_unless
        .as_deref()
        .unwrap()
        .collection()
        .unwrap()
        .hidden_discards
        .iter()
        .any(|(seat, pin)| *seat == P1 && pin.object_id == card));
}

/// S. A mid-collection state round-trips through serde unchanged.
#[test]
fn viper_mid_collection_state_round_trips_through_serde() {
    let (mut runner, _) = viper_attacking_with_blight_counters(3, 1);
    let card = object_named(&runner, "Filler Card 1");
    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card);
    assert!(poll(&runner).is_some(), "reach guard: mid-collection");

    // Serde-skipped caches are rebuilt on load, so the wire form is what must
    // round-trip, together with the poll, its prompt and its resolving entry.
    let wire = serde_json::to_value(runner.state()).expect("the state serializes");
    let decoded: GameState = serde_json::from_value(wire.clone()).expect("the state decodes");
    assert_eq!(serde_json::to_value(&decoded).unwrap(), wire);
    assert_eq!(
        decoded.pending_simultaneous_unless,
        runner.state().pending_simultaneous_unless
    );
    assert_eq!(decoded.waiting_for, runner.state().waiting_for);
    assert_eq!(
        decoded.resolving_stack_entry,
        runner.state().resolving_stack_entry
    );
}

/// S2. CR 117.2e: a save taken while the poll holds its resolution restores
/// (the wait is not a priority wait) and the poll finishes on the restored game.
#[test]
fn viper_poll_restored_mid_collection_finishes() {
    let (mut runner, viper) = viper_attacking_with_blight_counters(2, 1);
    decline(&mut runner);
    assert_poll_holds_its_resolution(&runner, viper, "P1's second prompt");

    let restored = round_trip(runner.state());
    assert_eq!(
        restored.pending_simultaneous_unless,
        runner.state().pending_simultaneous_unless
    );
    assert_eq!(restored.waiting_for, runner.state().waiting_for);
    assert_eq!(
        restored
            .resolving_stack_entry
            .as_ref()
            .map(|entry| entry.id),
        runner
            .state()
            .resolving_stack_entry
            .as_ref()
            .map(|entry| entry.id)
    );

    let mut restored = GameRunner::from_state(restored);
    decline(&mut restored);
    assert_eq!(restored.life(P1), 12);
    assert_poll_released(&restored, P0, "after the restored decline");
}

// ---------------------------------------------------------------------------
// CR 101.4 + CR 614.1 + CR 603.10: the delivery order and its held events.
// ---------------------------------------------------------------------------

/// The wait names `player`'s CR 616.1 replacement choice.
fn expect_replacement_choice(runner: &GameRunner, player: PlayerId, context: &str) {
    match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player: chooser, ..
        } => {
            assert_eq!(*chooser, player, "{context}: whose CR 616.1 choice")
        }
        other => panic!("{context}: expected {player:?}'s CR 616.1 choice, got {other:?}"),
    }
}

/// Answer the open CR 616.1 choice with its first candidate.
fn answer_replacement_choice(runner: &mut GameRunner) {
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the replacement choice is answered");
}

/// C3a. CR 101.2 + CR 701.21a: a permanent that can't be sacrificed (Alexios,
/// Deimos of Kosmos: "can't be sacrificed") is not a choice for the Viper's
/// sacrifice branch, so a player whose only permanent it is has nothing to pay
/// with and is not prompted.
#[test]
fn viper_unless_pool_excludes_a_permanent_that_cant_be_sacrificed() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = bare_viper_board(2, 0);
    let alexios = scenario.add_real_card(P1, "Alexios, Deimos of Kosmos", Zone::Battlefield, db);
    let runner = viper_attacks(scenario, viper);

    assert_eq!(
        zone_of(&runner, alexios),
        Zone::Battlefield,
        "reach guard: Alexios is on the battlefield"
    );
    assert!(
        poll(&runner).is_none(),
        "P1 is never prompted: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.life(P1), 16, "the instance went unpaid");
    assert_poll_released(&runner, P0, "no prompt was raised");
}

/// C3b. Paired reach guard for C3a: beside a permanent that can't be sacrificed,
/// the sacrificeable one is offered and the other is not.
#[test]
fn viper_unless_pool_offers_the_sacrificeable_permanent_beside_one_that_cant_be() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = bare_viper_board(2, 0);
    let alexios = scenario.add_real_card(P1, "Alexios, Deimos of Kosmos", Zone::Battlefield, db);
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = viper_attacks(scenario, viper);

    choose_branch(&mut runner, Branch::Sacrifice);
    match &runner.state().waiting_for {
        WaitingFor::WardSacrificeChoice {
            player, permanents, ..
        } => {
            assert_eq!(*player, P1);
            assert!(permanents.contains(&bears), "the Bears are offered");
            assert!(
                !permanents.contains(&alexios),
                "Alexios can't be sacrificed: {permanents:?}"
            );
        }
        other => panic!("expected P1's sacrifice pick, got {other:?}"),
    }
    pick(&mut runner, bears);

    assert_eq!(zone_of(&runner, bears), Zone::Graveyard);
    assert_eq!(zone_of(&runner, alexios), Zone::Battlefield);
    assert_eq!(runner.life(P1), 20);
}

/// (a). CR 101.4 + CR 614.1: Rest in Peace is sacrificed for one instance while
/// another player's chosen discard is delivered. The discard happens as part of
/// the same simultaneous event, with Rest in Peace still on the battlefield, so
/// the card is exiled rather than put into the graveyard.
#[test]
fn viper_rest_in_peace_sacrificed_for_one_instance_still_exiles_another_players_discard() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(3, 0);
    let rip = scenario.add_real_card(P1, "Rest in Peace", Zone::Battlefield, db);
    let mut runner = viper_attacks(scenario, viper);
    let card = object_named(&runner, "Filler Card 2");

    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, rip);
    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card);

    assert_eq!(
        zone_of(&runner, card),
        Zone::Exile,
        "Rest in Peace still applied to the discard"
    );
    assert_eq!(zone_of(&runner, rip), Zone::Exile, "its own sacrifice too");
    assert_eq!((runner.life(P1), runner.life(P2)), (20, 20));
    assert_poll_released(&runner, P0, "after the last pick");
}

/// (b). CR 101.4 + CR 101.2: Platinum Emperion ("Your life total can't change.")
/// is sacrificed for one instance while its controller declines the other. The
/// life loss is part of the same simultaneous event, so it still has no effect.
/// P2's own unpaid instances prove the life-loss steps ran in this delivery.
#[test]
fn viper_platinum_emperion_sacrificed_for_one_instance_still_keeps_its_life_total() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(3, 1);
    let emperion = scenario.add_real_card(P1, "Platinum Emperion", Zone::Battlefield, db);
    let mut runner = viper_attacks(scenario, viper);

    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, emperion);
    decline(&mut runner);
    decline(&mut runner);
    decline(&mut runner);

    assert_eq!(runner.life(P1), 20, "Platinum Emperion's controller");
    assert_eq!(
        runner.life(P2),
        12,
        "reach guard: the unpaid instances were delivered"
    );
    assert_eq!(zone_of(&runner, emperion), Zone::Graveyard);
    assert_poll_released(&runner, P0, "after the last decline");
}

/// T1. CR 603.10: P1's discard completes and P2's sacrifice then pauses on its
/// CR 616.1 choice. The completed discard is part of the same simultaneous
/// event, so it is held, and Megrim's trigger is collected once, after the
/// sacrifice batch completes.
#[test]
fn viper_discard_completed_before_a_paused_sacrifice_still_triggers_megrim() {
    let Some((mut runner, megrim)) = viper_megrim_board(3, 0) else {
        return;
    };
    let card = object_named(&runner, "Filler Card 1");
    let bears = object_named(&runner, "Grizzly Bears 2");

    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card);
    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, bears);

    expect_replacement_choice(&runner, P2, "P2's Bears");
    assert_eq!(zone_of(&runner, card), Zone::Exile, "reach guard");
    assert_eq!(zone_of(&runner, bears), Zone::Battlefield, "reach guard");
    assert_eq!(triggers_from(&runner, megrim), 0, "held until completion");
    answer_replacement_choice(&mut runner);

    assert_eq!(triggers_from(&runner, megrim), 1);
    runner.advance_until_stack_empty();
    assert_eq!(
        (runner.life(P1), runner.life(P2), runner.life(P0)),
        (18, 20, 20)
    );
}

/// T2. CR 603.10: Megrim sacrificed in the delivery does not exist immediately
/// after it, so it does not trigger on the delivery's discards; the surviving
/// Megrim does, once per discard.
#[test]
fn viper_megrim_sacrificed_in_the_delivery_does_not_trigger_on_its_discards() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(4, 0);
    let m0 = scenario.add_real_card(P0, "Megrim", Zone::Battlefield, db);
    let m1 = scenario.add_real_card(P1, "Megrim", Zone::Battlefield, db);
    scenario.add_real_card(P2, "Leyline of the Void", Zone::Battlefield, db);
    scenario.add_real_card(P3, "Rest in Peace", Zone::Battlefield, db);
    let mut runner = viper_attacks(scenario, viper);
    let card2 = object_named(&runner, "Filler Card 2");
    let card3 = object_named(&runner, "Filler Card 3");

    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, m1);
    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card2);
    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card3);

    expect_replacement_choice(&runner, P3, "P3's card meets Leyline and Rest in Peace");
    assert_eq!(zone_of(&runner, card2), Zone::Exile, "reach guard");
    assert_eq!(zone_of(&runner, m1), Zone::Battlefield, "reach guard");
    assert_eq!(triggers_from(&runner, m0) + triggers_from(&runner, m1), 0);
    answer_replacement_choice(&mut runner);
    expect_replacement_choice(&runner, P1, "the sacrificed Megrim");
    answer_replacement_choice(&mut runner);

    assert_eq!(zone_of(&runner, m1), Zone::Exile);
    assert_eq!(triggers_from(&runner, m0), 2, "one per discard");
    assert_eq!(triggers_from(&runner, m1), 0, "it left in the delivery");
    runner.advance_until_stack_empty();
    assert_eq!(
        (runner.life(P2), runner.life(P3), runner.life(P1)),
        (18, 18, 20)
    );
}

/// T3. CR 603.10: Exquisite Blood sacrificed in the delivery does not trigger on
/// the delivery's life loss; the surviving one does.
#[test]
fn viper_exquisite_blood_survivor_triggers_and_the_sacrificed_one_does_not() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(3, 0);
    let b0 = scenario.add_real_card(P0, "Exquisite Blood", Zone::Battlefield, db);
    let b1 = scenario.add_real_card(P1, "Exquisite Blood", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Leyline of the Void", Zone::Battlefield, db);
    scenario.add_real_card(P2, "Rest in Peace", Zone::Battlefield, db);
    let mut runner = viper_attacks(scenario, viper);

    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, b1);
    decline(&mut runner);

    expect_replacement_choice(&runner, P1, "the sacrificed Exquisite Blood");
    assert_eq!(runner.life(P2), 16, "reach guard: the life loss happened");
    assert_eq!(zone_of(&runner, b1), Zone::Battlefield, "reach guard");
    assert_eq!(triggers_from(&runner, b0), 0, "held until completion");
    answer_replacement_choice(&mut runner);

    assert_eq!(triggers_from(&runner, b0), 1);
    assert_eq!(triggers_from(&runner, b1), 0);
    runner.advance_until_stack_empty();
    assert_eq!(
        (runner.life(P0), runner.life(P1), runner.life(P2)),
        (24, 20, 16)
    );
}

/// (d). C4: every opponent declines eight instances, settled by the answers'
/// own delivery. Each unpaid instance is delivered flat at constant depth, so
/// no chain of 24 nodes exceeds the resolution depth limit.
#[test]
fn viper_many_unpaid_instances_settled_by_answers_deliver_flat() {
    let (mut scenario, viper) = viper_board(4, 7);
    for seat in [P1, P2, P3] {
        scenario.with_life(seat, 100);
    }
    let mut runner = viper_attacks(scenario, viper);

    for _ in 0..24 {
        decline(&mut runner);
    }

    for seat in [P1, P2, P3] {
        assert_eq!(runner.life(seat), 68, "{seat:?}: eight unpaid instances");
    }
    assert_poll_released(&runner, P0, "after the last decline");
}

/// (d2). C4 at the start entry: no seat can pay, so the walk completes without a
/// prompt and the delivery runs from `start`.
#[test]
fn viper_many_unpaid_instances_with_no_payable_branch_deliver_flat() {
    let (mut scenario, viper) = bare_viper_board(4, 7);
    for seat in [P1, P2, P3] {
        scenario.with_life(seat, 100);
    }
    let runner = viper_attacks(scenario, viper);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::UnlessPayment { .. }
                | WaitingFor::UnlessPaymentChooseCost { .. }
                | WaitingFor::WardSacrificeChoice { .. }
                | WaitingFor::WardDiscardChoice { .. }
        ),
        "reach guard: no prompt was raised, got {:?}",
        runner.state().waiting_for
    );
    for seat in [P1, P2, P3] {
        assert_eq!(runner.life(seat), 68, "{seat:?}: eight unpaid instances");
    }
}

/// The (e) board: Leyline of the Void under P0, Rest in Peace under P2, and a
/// second Rest in Peace (`rip1`) under P1. P1 sacrifices `rip1`; P2 and P3
/// discard. P2's and P3's cards each meet Leyline and both Rest in Peaces, and
/// `rip1` meets all three, so each pauses on its owner's CR 616.1 choice.
fn viper_rip_delivery_board() -> Option<(GameRunner, ObjectId, ObjectId, ObjectId)> {
    let db = shared_card_db()?;
    let (mut scenario, viper) = viper_board(4, 0);
    scenario.add_real_card(P0, "Leyline of the Void", Zone::Battlefield, db);
    scenario.add_real_card(P2, "Rest in Peace", Zone::Battlefield, db);
    let rip1 = scenario.add_real_card(P1, "Rest in Peace", Zone::Battlefield, db);
    let mut runner = viper_attacks(scenario, viper);
    let card2 = object_named(&runner, "Filler Card 2");
    let card3 = object_named(&runner, "Filler Card 3");

    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, rip1);
    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card2);
    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card3);
    Some((runner, rip1, card2, card3))
}

/// (e). C4 + C5 for the cursor: the discards are delivered first, so P2's
/// CR 616.1 choice comes before P1's (HEAD sacrificed first). The pending
/// discard of P3 stays hidden from every viewer but P3 while it waits.
#[test]
fn viper_paused_discard_delivers_the_sacrifices_after_the_answer() {
    let Some((mut runner, rip1, card2, card3)) = viper_rip_delivery_board() else {
        return;
    };

    expect_replacement_choice(&runner, P2, "P2's card");
    assert_eq!(
        zone_of(&runner, rip1),
        Zone::Battlefield,
        "reach guard: the sacrifice is delivered last"
    );
    let pending = delivery(&runner).expect("the delivery is parked");
    assert_eq!(
        pending
            .discards
            .iter()
            .map(|(seat, pin)| (*seat, pin.object_id))
            .collect::<Vec<_>>(),
        vec![(P3, card3)],
        "P3's pick waits"
    );
    let p3_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P3);
    assert_eq!(
        p3_view
            .pending_simultaneous_unless
            .as_deref()
            .and_then(PendingSimultaneousUnless::delivery)
            .map(|d| d.discards.len()),
        Some(1),
        "P3 keeps its own pick"
    );
    for viewer in [P0, P1, P2] {
        let view = engine::game::visibility::filter_state_for_viewer(runner.state(), viewer);
        let projected = view
            .pending_simultaneous_unless
            .as_deref()
            .and_then(PendingSimultaneousUnless::delivery)
            .expect("the projection keeps the delivery");
        assert!(projected.discards.is_empty(), "{viewer:?} sees no pick");
        if viewer == P0 {
            // The carrier's validating decode admits a viewer projection.
            serde_json::from_value::<PendingSimultaneousUnless>(
                serde_json::to_value(view.pending_simultaneous_unless.as_deref()).unwrap(),
            )
            .expect("a mid-delivery viewer projection of the poll decodes");
        }
    }
    let unseated = engine::game::visibility::filter_state_for_unseated_viewer(runner.state());
    assert!(unseated
        .pending_simultaneous_unless
        .as_deref()
        .and_then(PendingSimultaneousUnless::delivery)
        .expect("the projection keeps the delivery")
        .discards
        .is_empty());

    answer_replacement_choice(&mut runner);
    expect_replacement_choice(&runner, P3, "P3's card");
    answer_replacement_choice(&mut runner);
    expect_replacement_choice(&runner, P1, "the sacrificed Rest in Peace");
    answer_replacement_choice(&mut runner);

    for object in [card2, card3, rip1] {
        assert_eq!(zone_of(&runner, object), Zone::Exile);
    }
    for seat in [P0, P1, P2, P3] {
        assert_eq!(runner.life(seat), 20, "{seat:?}");
    }
    assert_poll_released(&runner, P0, "after the last replacement answer");
}

/// (e2). A delivery paused mid-way holds its completed events, serializes, and
/// finishes after a restore with each trigger exactly once.
#[test]
fn viper_delivery_paused_mid_way_restores_and_finishes() {
    let Some((runner, megrim)) = viper_megrim_board(3, 0) else {
        return;
    };
    let mut runner = runner;
    let p1_card = object_named(&runner, "Filler Card 1");
    let p2_card = object_named(&runner, "Filler Card 2");
    for card in [p1_card, p2_card] {
        choose_branch(&mut runner, Branch::Discard);
        pick(&mut runner, card);
    }

    assert_paused_after_one_completed_discard(&runner, megrim, p1_card, p2_card, "the pick");
    assert!(
        delivery(&runner)
            .expect("the delivery is parked")
            .held_events
            .iter()
            .any(|event| matches!(
                event,
                GameEvent::ZoneChanged { object_id, .. } if *object_id == p1_card
            )),
        "reach guard: P1's completed discard is held"
    );

    let restored = round_trip(runner.state());
    assert_eq!(
        restored.pending_simultaneous_unless,
        runner.state().pending_simultaneous_unless
    );
    assert_eq!(restored.waiting_for, runner.state().waiting_for);
    let mut restored = GameRunner::from_state(restored);
    answer_the_paused_discard(&mut restored, megrim, p2_card);
    assert_eq!(
        (restored.life(P1), restored.life(P2), restored.life(P0)),
        (18, 18, 20)
    );
}

/// (f1). CR 800.4a + CR 800.4f: an unpaid seat conceding during a paused
/// delivery is dropped from the cursor and loses no life.
#[test]
fn viper_unpaid_seat_conceding_during_a_paused_delivery_is_dropped() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(4, 0);
    scenario.add_real_card(P0, "Leyline of the Void", Zone::Battlefield, db);
    scenario.add_real_card(P2, "Rest in Peace", Zone::Battlefield, db);
    let mut runner = viper_attacks(scenario, viper);
    let bears = object_named(&runner, "Grizzly Bears 1");
    let card = object_named(&runner, "Filler Card 2");

    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, bears);
    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card);
    decline(&mut runner);

    expect_replacement_choice(&runner, P2, "P2's card");
    assert_eq!(
        delivery(&runner).expect("parked").unpaid,
        vec![P3],
        "reach guard: P3's unpaid instance waits"
    );
    concede(&mut runner, P3);
    assert!(is_eliminated(&runner, P3));
    assert!(delivery(&runner).expect("still parked").unpaid.is_empty());

    answer_replacement_choice(&mut runner);
    expect_replacement_choice(&runner, P1, "the sacrificed Bears");
    answer_replacement_choice(&mut runner);

    assert_eq!(zone_of(&runner, card), Zone::Exile);
    assert_eq!(zone_of(&runner, bears), Zone::Exile);
    assert_eq!((runner.life(P1), runner.life(P2)), (20, 20));
    assert_poll_released(&runner, P0, "after the last replacement answer");
}

/// (f2). CR 800.4a + CR 118.12a: P1's chosen permanent is P3's Grizzly Bears,
/// borrowed with Mind Control. P3 leaves during a paused delivery, the Bears
/// leave with it, and the stale pick is skipped (its instance still counts as
/// paid) instead of failing the sacrifice.
#[test]
fn viper_borrowed_permanent_leaving_with_its_owner_during_a_paused_delivery_is_skipped() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(4, 0);
    scenario.add_real_card(P0, "Leyline of the Void", Zone::Battlefield, db);
    scenario.add_real_card(P2, "Rest in Peace", Zone::Battlefield, db);
    let control = scenario
        .add_enchantment_from_oracle(P1, "Mind Control", MIND_CONTROL)
        .with_subtypes(vec!["Aura"])
        .id();
    let mut runner = scenario.build();
    let stolen = object_named(&runner, "Grizzly Bears 3");
    {
        let state = runner.state_mut();
        state.objects.get_mut(&control).unwrap().attached_to = Some(stolen.into());
        state
            .objects
            .get_mut(&stolen)
            .unwrap()
            .attachments
            .push(control);
        state.layers_dirty.mark_full();
    }
    engine::game::layers::flush_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&stolen].controller,
        P1,
        "reach guard: P1 controls P3's Bears"
    );
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(viper, AttackTarget::Player(P1))])
        .expect("Viper attacks P1");
    runner.advance_until_stack_empty();
    let card = object_named(&runner, "Filler Card 2");

    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, stolen);
    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card);
    decline(&mut runner);

    expect_replacement_choice(&runner, P2, "P2's card");
    assert_eq!(
        zone_of(&runner, stolen),
        Zone::Battlefield,
        "reach guard: the borrowed Bears are still on the battlefield"
    );
    concede(&mut runner, P3);
    assert_ne!(
        zone_of(&runner, stolen),
        Zone::Battlefield,
        "the Bears left with their owner (CR 800.4a)"
    );

    answer_replacement_choice(&mut runner);
    assert!(poll(&runner).is_none(), "the delivery finished");
    assert_ne!(zone_of(&runner, stolen), Zone::Battlefield);
    // CR 704.5m: Mind Control is attached to nothing and is put into the
    // graveyard; Leyline of the Void and Rest in Peace both apply (CR 616.1).
    expect_replacement_choice(&runner, P1, "Mind Control");
    answer_replacement_choice(&mut runner);
    assert_eq!(zone_of(&runner, control), Zone::Exile);
    assert_poll_released(&runner, P0, "after the last answer");
    assert_eq!(runner.life(P1), 20, "the instance still counted as paid");
}

/// (f4). CR 800.4a + CR 701.9a: the chooser of the paused step concedes. Its
/// card left the game and was never discarded; the delivery resumes inside the
/// concession, and P1's held discard is collected exactly once.
#[test]
fn viper_step_chooser_conceding_during_a_paused_delivery_resumes_it() {
    let Some((mut runner, megrim)) = viper_megrim_board(4, 0) else {
        return;
    };
    let p1_card = object_named(&runner, "Filler Card 1");
    let p2_card = object_named(&runner, "Filler Card 2");
    for card in [p1_card, p2_card] {
        choose_branch(&mut runner, Branch::Discard);
        pick(&mut runner, card);
    }
    decline(&mut runner);

    expect_replacement_choice(&runner, P2, "P2's card");
    assert_eq!(zone_of(&runner, p1_card), Zone::Exile, "reach guard");
    assert_eq!(delivery(&runner).expect("parked").unpaid, vec![P3]);
    assert_eq!(triggers_from(&runner, megrim), 0, "reach guard: held");
    assert!(runner.state().pending_discard_batch.is_some());

    concede(&mut runner, P2);

    assert!(is_eliminated(&runner, P2));
    assert!(poll(&runner).is_none(), "the delivery resumed and finished");
    assert!(runner.state().pending_discard_batch.is_none());
    assert_eq!(runner.life(P3), 16, "the unpaid step ran in the concession");
    assert_eq!(
        triggers_from(&runner, megrim),
        1,
        "P1's held discard was collected exactly once; P2's card never was"
    );
    runner.advance_until_stack_empty();
    assert_eq!((runner.life(P1), runner.life(P0)), (18, 20));
    assert!(runner.state().resolving_stack_entry.is_none());
}

/// (f4b). CR 800.4a + CR 701.21a: the sacrificing chooser concedes while its
/// sacrifice step waits on its CR 616.1 choice (Rest in Peace against Leyline of
/// the Void). The permanent left the game with its owner, so it is not
/// sacrificed; the discard that already happened is held across the pause and
/// collected exactly once, and the unpaid step's life loss lands once. The
/// sacrifice step is the delivery's last, so its carrier is already consumed;
/// (f4) guards the delivery's own departure path.
#[test]
fn viper_sacrificing_chooser_conceding_during_its_paused_sacrifice_completes_the_batch() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(4, 0);
    let megrim = scenario.add_real_card(P0, "Megrim", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Leyline of the Void", Zone::Battlefield, db);
    let rip = scenario.add_real_card(P1, "Rest in Peace", Zone::Battlefield, db);
    let mut runner = viper_attacks(scenario, viper);
    let p2_card = object_named(&runner, "Filler Card 2");

    choose_branch(&mut runner, Branch::Sacrifice);
    pick(&mut runner, rip);
    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, p2_card);
    decline(&mut runner);

    // P2's card meets both replacements first; its answer completes the discard.
    expect_replacement_choice(&runner, P2, "P2's discard");
    answer_replacement_choice(&mut runner);

    expect_replacement_choice(&runner, P1, "the sacrificed Rest in Peace");
    assert_eq!(zone_of(&runner, p2_card), Zone::Exile, "reach guard");
    assert_eq!(
        zone_of(&runner, rip),
        Zone::Battlefield,
        "reach guard: the sacrifice is still waiting"
    );
    assert_eq!(triggers_from(&runner, megrim), 0, "reach guard: held");
    assert_eq!(
        runner.life(P3),
        16,
        "reach guard: the unpaid step is delivered before the sacrifice step"
    );
    assert!(
        runner
            .state()
            .pending_player_scope_sacrifice_choice
            .is_some(),
        "reach guard: the sacrifice batch is parked on the replacement choice"
    );
    assert!(
        poll(&runner).is_none(),
        "reach guard: the sacrifice step is the delivery's last; its carrier is already consumed"
    );

    let concession = runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("a concession is accepted");

    assert!(is_eliminated(&runner, P1));
    assert_ne!(
        zone_of(&runner, rip),
        Zone::Battlefield,
        "Rest in Peace left the game with its owner"
    );
    assert!(
        concession.events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, .. } if *object_id == rip
        )),
        "reach guard: the departure moved Rest in Peace"
    );
    assert!(
        !concession.events.iter().any(|event| matches!(
            event,
            GameEvent::PermanentSacrificed { object_id, .. } if *object_id == rip
        )),
        "it left with its owner (CR 800.4a); it was not sacrificed"
    );
    assert!(runner
        .state()
        .pending_player_scope_sacrifice_choice
        .is_none());
    assert!(runner.state().pending_replacement.is_none());
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "no replacement choice outlives its chooser"
    );
    assert_eq!(runner.life(P3), 16, "the unpaid step landed once");
    assert_eq!(
        triggers_from(&runner, megrim),
        1,
        "P2's discard was collected exactly once"
    );
    assert_eq!(stack_entries_from(&runner, megrim), 1);
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P2), 18, "Megrim dealt 2 to P2 once");
    assert_eq!(runner.life(P3), 16);
    assert_eq!(runner.life(P0), 20);
}

/// (f5). CR 800.4a: the Viper's controller concedes during a paused delivery.
/// The resolving ability ceased to exist, so the remaining steps don't happen,
/// but the events that already happened are released and collected once.
#[test]
fn viper_controller_conceding_during_a_paused_delivery_releases_its_held_events() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, viper) = viper_board(4, 0);
    let m3 = scenario.add_real_card(P3, "Megrim", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Leyline of the Void", Zone::Battlefield, db);
    scenario.add_real_card(P2, "Rest in Peace", Zone::Battlefield, db);
    let mut runner = viper_attacks(scenario, viper);
    let p1_card = object_named(&runner, "Filler Card 1");
    let p2_card = object_named(&runner, "Filler Card 2");
    for card in [p1_card, p2_card] {
        choose_branch(&mut runner, Branch::Discard);
        pick(&mut runner, card);
    }
    decline(&mut runner);

    expect_replacement_choice(&runner, P2, "P2's card");
    assert_eq!(zone_of(&runner, p1_card), Zone::Exile, "reach guard");
    assert_eq!(delivery(&runner).expect("parked").unpaid, vec![P3]);
    assert_eq!(triggers_from(&runner, m3), 0, "reach guard: held");

    concede(&mut runner, P0);

    assert!(is_eliminated(&runner, P0));
    assert!(poll(&runner).is_none());
    assert_eq!(
        runner.life(P3),
        20,
        "the remaining unpaid step did not happen"
    );
    assert_eq!(
        triggers_from(&runner, m3),
        1,
        "P1's held discard was released and collected once"
    );
    // CR 616.1 + CR 614.1: P2's discard is already under way and P2 is still in
    // the game, so it completes on P2's answer.
    expect_replacement_choice(&runner, P2, "the in-progress discard");
    assert!(runner.state().pending_discard_batch.is_some());
    answer_replacement_choice(&mut runner);
    assert_eq!(zone_of(&runner, p2_card), Zone::Exile);
    assert_eq!(triggers_from(&runner, m3), 2);
    runner.advance_until_stack_empty();
    assert_eq!(
        (runner.life(P1), runner.life(P2), runner.life(P3)),
        (18, 18, 20)
    );
    assert!(runner.state().resolving_stack_entry.is_none());
}

/// C5. CR 400.2: the second discard prompt lists the chooser's candidate hand
/// cards; no other player's projection receives them.
#[test]
fn viper_second_discard_prompt_hides_the_candidate_cards_from_other_players() {
    let (mut scenario, viper) = viper_board(2, 1);
    scenario.with_cards_in_hand(P1, &["Filler Card B"]);
    let mut runner = viper_attacks(scenario, viper);
    let card_a = object_named(&runner, "Filler Card 1");
    let card_b = object_named(&runner, "Filler Card B");

    choose_branch(&mut runner, Branch::Discard);
    pick(&mut runner, card_a);
    choose_branch(&mut runner, Branch::Discard);

    match &runner.state().waiting_for {
        WaitingFor::WardDiscardChoice { player, cards, .. } => {
            assert_eq!(*player, P1);
            assert_eq!(cards, &vec![card_b], "reach guard: the second pick is open");
        }
        other => panic!("expected P1's discard pick, got {other:?}"),
    }
    let candidates = |viewer: PlayerId| match engine::game::visibility::filter_state_for_viewer(
        runner.state(),
        viewer,
    )
    .waiting_for
    {
        WaitingFor::WardDiscardChoice { cards, .. } => cards,
        other => panic!("the projection keeps the prompt, got {other:?}"),
    };
    assert!(candidates(P0).is_empty(), "the opponent sees no candidates");
    assert_eq!(candidates(P1), vec![card_b], "the chooser keeps them");
}

/// C8. Positive control for V10: an effect-caused discard (Mind Rot) does offer
/// Library of Leng's replacement, so V10's graveyard result comes from the
/// cost/effect distinction and not from Library of Leng being inert.
#[test]
fn library_of_leng_offers_the_library_top_for_an_effect_caused_discard() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let (mut scenario, _viper) = viper_board(2, 0);
    scenario.add_real_card(P1, "Library of Leng", Zone::Battlefield, db);
    let mind_rot = scenario.add_real_card(P0, "Mind Rot", Zone::Hand, db);
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    let mut runner = scenario.build();
    let card = object_named(&runner, "Filler Card 1");

    runner.cast(mind_rot).target_player(P1).resolve();

    let candidates = match &runner.state().waiting_for {
        WaitingFor::ReplacementChoice {
            player, candidates, ..
        } => {
            assert_eq!(*player, P1, "Library of Leng's controller chooses");
            candidates.clone()
        }
        other => panic!("expected Library of Leng's optional replacement, got {other:?}"),
    };
    let accept = candidates
        .iter()
        .position(|candidate| candidate.description != "Decline")
        .expect("the replacement is offered beside its decline");
    runner
        .act(GameAction::ChooseReplacement { index: accept })
        .expect("Library of Leng's replacement is accepted");

    assert_eq!(
        runner.state().players[1].library.iter().next().copied(),
        Some(card),
        "the discarded card is the top card of P1's library"
    );
}

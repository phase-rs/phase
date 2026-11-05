//! CR 118.8 + CR 207.2d: an additional-cost line printed behind a flavor word
//! is the spell's additional cost. The flavor word has no rules meaning, so
//! the line casts exactly as the unlabeled line does.
//!
//! Redemptor Dreadnought (Oracle text, `data/mtgjson/AtomicCards.json`):
//!   "Fallen Warrior — As an additional cost to cast this spell, you may exile
//!    a creature card from your graveyard.
//!    Trample
//!    Plasma Incinerator — Whenever this creature attacks, if a card is exiled
//!    with it, it gets +X/+X until end of turn, where X is the power of the
//!    exiled card."
//!
//! Every row drives the real cast pipeline. Builder settings that are not
//! Oracle text: mana cost {0}, so the cast needs no mana.

use engine::game::combat::AttackTarget;
use engine::game::players::linked_exile_cards_for_source;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::turns::start_next_turn;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Redemptor Dreadnought's printed Oracle text.
const REDEMPTOR_DREADNOUGHT: &str = "Fallen Warrior — As an additional cost to cast this spell, \
you may exile a creature card from your graveyard.\n\
Trample\n\
Plasma Incinerator — Whenever this creature attacks, if a card is exiled with it, it gets +X/+X \
until end of turn, where X is the power of the exiled card.";

/// The Dreadnought in P0's hand and a 5/5 creature card in P0's graveyard.
struct Board {
    scenario: GameScenario,
    dreadnought: ObjectId,
    giant: ObjectId,
}

fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dreadnought = scenario
        .add_creature_to_hand_from_oracle(P0, "Redemptor Dreadnought", 4, 4, REDEMPTOR_DREADNOUGHT)
        .with_mana_cost(ManaCost::zero())
        .as_artifact_creature()
        .id();
    let giant = scenario
        .add_creature_to_graveyard(P0, "Graveyard Giant", 5, 5)
        .id();
    Board {
        scenario,
        dreadnought,
        giant,
    }
}

/// Announce the Dreadnought's cast and check the optional cost is offered.
fn announce_cast(runner: &mut GameRunner, dreadnought: ObjectId) {
    let card_id = runner.state().objects[&dreadnought].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: dreadnought,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the Dreadnought");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "CR 118.8b + CR 601.2b: the cast offers Fallen Warrior's optional cost, got {:?}",
        runner.state().waiting_for
    );
}

fn settled(state: &GameState) -> bool {
    matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty()
}

/// Pass priority until the stack is empty. Panics on any other prompt.
fn resolve_stack(runner: &mut GameRunner) {
    for _ in 0..64 {
        if settled(runner.state()) {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the top of the stack");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("the stack did not empty");
}

fn linked_pile(runner: &GameRunner, source: ObjectId) -> Vec<ObjectId> {
    linked_exile_cards_for_source(runner.state(), source)
        .iter()
        .map(|linked| linked.exiled_id)
        .collect()
}

/// Move to P0's next turn (the opponent's turn, then P0's) and attack P1.
fn attack_on_next_turn(runner: &mut GameRunner, attacker: ObjectId) {
    let mut events = Vec::new();
    start_next_turn(runner.state_mut(), &mut events);
    start_next_turn(runner.state_mut(), &mut events);
    assert_eq!(runner.state().active_player, P0, "setup: P0's next turn");
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare the attacker");
}

fn attack_trigger_on_stack(runner: &GameRunner, source: ObjectId) -> bool {
    runner.state().stack.iter().any(|entry| {
        matches!(
            entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if source_id == source
        )
    })
}

fn power_toughness(runner: &GameRunner, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let object = &runner.state().objects[&id];
    (object.power, object.toughness)
}

/// K1a. CR 118.8b + CR 601.2b: the printed line offers its optional cost.
#[test]
fn printed_dreadnought_offers_fallen_warrior_cost() {
    let Board {
        scenario,
        dreadnought,
        ..
    } = board();
    let mut runner = scenario.build();
    announce_cast(&mut runner, dreadnought);
}

/// K1b. CR 118.8 + CR 601.2h: paying the cost exiles the chosen graveyard card.
#[test]
fn paying_fallen_warrior_exiles_the_chosen_graveyard_card() {
    let Board {
        scenario,
        dreadnought,
        giant,
    } = board();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(dreadnought)
        .accept_optional()
        .pay_cost_with(&[giant])
        .resolve();

    outcome.assert_zone(&[dreadnought], Zone::Battlefield);
    outcome.assert_zone(&[giant], Zone::Exile);
}

/// K1c. CR 118.8b: the cost is optional; declining it exiles nothing.
#[test]
fn declining_fallen_warrior_exiles_nothing() {
    let Board {
        scenario,
        dreadnought,
        giant,
    } = board();
    let mut runner = scenario.build();
    announce_cast(&mut runner, dreadnought);

    runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("decline the optional cost");
    resolve_stack(&mut runner);

    assert_eq!(runner.state().objects[&dreadnought].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&giant].zone, Zone::Graveyard);
    assert!(linked_pile(&runner, dreadnought).is_empty());
}

/// K1d. The cost names a creature card from your graveyard: a land card in
/// your graveyard and a creature card in the opponent's graveyard are not
/// choices.
#[test]
fn fallen_warrior_offers_only_creature_cards_from_your_graveyard() {
    let Board {
        mut scenario,
        dreadnought,
        giant,
    } = board();
    scenario.add_land_to_graveyard(P0, "Forest");
    scenario.add_creature_to_graveyard(P1, "Opponent's Giant", 6, 6);
    let mut runner = scenario.build();
    announce_cast(&mut runner, dreadnought);

    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("pay the optional cost");

    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::PayCost { choices, .. } if choices == &vec![giant]
        ),
        "only P0's creature card is a choice, got {:?}",
        runner.state().waiting_for
    );
}

/// D1. CR 607.2q + CR 400.7d + CR 603.4: the card exiled for
/// Fallen Warrior is exiled with the Dreadnought, so a later turn's attack
/// gives +5/+5.
#[test]
fn card_exiled_for_fallen_warrior_pumps_a_later_turn_attack() {
    let Board {
        scenario,
        dreadnought,
        giant,
    } = board();
    let mut runner = scenario.build();
    runner
        .cast(dreadnought)
        .accept_optional()
        .pay_cost_with(&[giant])
        .resolve();
    assert_eq!(runner.state().objects[&giant].zone, Zone::Exile);

    attack_on_next_turn(&mut runner, dreadnought);
    assert_eq!(
        linked_pile(&runner, dreadnought),
        vec![giant],
        "reach guard: CR 607.2q, the cost exile is linked to the permanent"
    );
    assert!(
        attack_trigger_on_stack(&runner, dreadnought),
        "reach guard: CR 603.4, a card is exiled with it, so the attack trigger is on the stack"
    );
    resolve_stack(&mut runner);

    assert_eq!(
        power_toughness(&runner, dreadnought),
        (Some(9), Some(9)),
        "CR 607.2q: X is the power of the card exiled with it (5), so 4/4 + 5/5"
    );
}

/// D1 hostile. CR 603.4: a declined cost leaves no card exiled with the
/// Dreadnought, so its attack trigger never goes on the stack.
#[test]
fn declined_fallen_warrior_gives_no_attack_pump() {
    let Board {
        scenario,
        dreadnought,
        giant,
    } = board();
    let mut runner = scenario.build();
    announce_cast(&mut runner, dreadnought);
    runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("decline the optional cost");
    resolve_stack(&mut runner);
    assert_eq!(runner.state().objects[&giant].zone, Zone::Graveyard);

    attack_on_next_turn(&mut runner, dreadnought);
    assert!(
        !attack_trigger_on_stack(&runner, dreadnought),
        "CR 603.4: no card is exiled with it, so the ability does not trigger"
    );
    resolve_stack(&mut runner);

    assert_eq!(power_toughness(&runner, dreadnought), (Some(4), Some(4)));
}

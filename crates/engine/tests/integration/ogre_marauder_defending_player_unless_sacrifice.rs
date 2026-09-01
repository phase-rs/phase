//! Production-path coverage for Ogre Marauder — the "defending player" payer
//! class of unless-costs.
//!
//! Oracle:
//!   "Whenever this creature attacks, it gains \"this creature can't be
//!    blocked\" until end of turn unless defending player sacrifices a creature
//!    of their choice."
//!
//! Reported bug: the trigger went on the stack, but the defending player could
//! still block Ogre Marauder even without sacrificing. The clause's subject —
//! "defending player" — was not in the unless-payer subject grammar, so the
//! whole clause fell through to an `Unsupported unless clause` gap: the trigger
//! resolved to nothing, no payment was demanded, and no grant was applied.
//!
//! Both branches are asserted here, because a fix that only grants the
//! unblockable ability (never offering the payment) and a fix that only demands
//! the sacrifice (never granting) each look correct from one side.
//!
//! CR ANCHORS (verified against docs/MagicCompRules.txt):
//!   * CR 118.12a — "[Do something] unless [a player does something else]."
//!   * CR 508.5 — an attacking creature's ability that refers to "defending
//!     player" means the player that creature is attacking.
//!   * CR 508.1m — abilities that trigger on attackers being declared trigger.
//!   * CR 509.1a — the defending player chooses which creatures block and what
//!     each one blocks.
//!   * CR 509.1b — a declaration that disobeys a block restriction is illegal.
//!   * CR 701.21a — to sacrifice a permanent, its controller moves it from the
//!     battlefield to its owner's graveyard.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const OGRE_MARAUDER: &str = "Whenever this creature attacks, it gains \
     \"this creature can't be blocked\" until end of turn unless defending \
     player sacrifices a creature of their choice.";

struct Board {
    runner: GameRunner,
    marauder: ObjectId,
    /// P1's would-be blocker.
    blocker: ObjectId,
    /// A second P1 creature, so the sacrifice has a choice that is not the
    /// blocker itself.
    fodder: ObjectId,
}

/// Build the board, attack with Ogre Marauder, and run the trigger up to the
/// point where the defending player must decide (CR 508.1m + CR 118.12a).
fn attack_and_reach_payment_prompt() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let marauder = scenario
        .add_creature_from_oracle(P0, "Ogre Marauder", 3, 1, OGRE_MARAUDER)
        .id();
    let blocker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let fodder = scenario.add_creature(P1, "Llanowar Elves", 1, 1).id();

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .declare_attackers(&[(marauder, AttackTarget::Player(P1))])
        .expect("Ogre Marauder may attack P1");
    runner.advance_until_stack_empty();

    Board {
        runner,
        marauder,
        blocker,
        fodder,
    }
}

/// True once the engine is at the declare-blockers decision.
fn at_declare_blockers(runner: &GameRunner) -> bool {
    matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    )
}

/// CR 509.1b: ask the engine's own block-declaration authority whether the
/// defending player may block `attacker` with `blocker`.
///
/// `validate_blockers_for_player` is the same predicate the `DeclareBlockers`
/// action runs, so this asserts the real restriction rather than a test-local
/// re-derivation — and, unlike driving the action, it does not depend on the
/// engine stopping at `WaitingFor::DeclareBlockers`, which it skips entirely
/// when the only attacker cannot be blocked.
fn block_is_legal(runner: &GameRunner, blocker: ObjectId, attacker: ObjectId) -> bool {
    engine::game::combat::validate_blockers_for_player(runner.state(), P1, &[(blocker, attacker)])
        .is_ok()
}

/// Pass priority until the declare-blockers decision is reached. Only valid on
/// the branch where a legal block exists — with the attacker unblockable the
/// engine skips the step.
fn advance_to_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..12 {
        if at_declare_blockers(runner) {
            return;
        }
        runner.pass_both_players();
    }
    panic!(
        "never reached declare blockers; stuck at {:?}",
        runner.state().waiting_for
    );
}

/// CR 508.5 + CR 118.12a: the attack trigger must prompt the DEFENDING player —
/// the player Ogre Marauder is attacking — for the sacrifice. Prompting the
/// attacker's controller (the `TriggeringPlayer` fallback the pronoun subjects
/// use) would let the attacker pay their own tax.
#[test]
fn attack_trigger_prompts_the_defending_player() {
    let board = attack_and_reach_payment_prompt();
    match board.runner.state().waiting_for {
        WaitingFor::UnlessPayment { player, .. } => assert_eq!(
            player, P1,
            "CR 508.5: the defending player pays, not the attacker's controller"
        ),
        ref other => panic!("attack trigger must offer the unless-payment, got {other:?}"),
    }
}

/// CR 118.12a + CR 509.1b: DECLINING the sacrifice applies the effect — Ogre
/// Marauder gains "this creature can't be blocked" until end of turn, so a
/// block declaration naming it is illegal. This is the reported bug: before the
/// fix the block was accepted even though nothing was sacrificed.
#[test]
fn declining_the_sacrifice_makes_ogre_marauder_unblockable() {
    let mut board = attack_and_reach_payment_prompt();

    board
        .runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("the defending player may decline the sacrifice");

    // Nothing was sacrificed: both P1 creatures are still on the battlefield.
    for id in [board.blocker, board.fodder] {
        assert_eq!(
            board.runner.state().objects[&id].zone,
            Zone::Battlefield,
            "declining must not sacrifice anything"
        );
    }

    // CR 509.1b: the grant is live, so no block declaration naming Ogre
    // Marauder is legal. This is the reported bug — before the fix the trigger
    // resolved to nothing and this exact block was accepted.
    assert!(
        engine::game::combat::has_cant_be_blocked_static(board.runner.state(), board.marauder),
        "declining must leave Ogre Marauder with the \"can't be blocked\" grant"
    );
    assert!(
        !block_is_legal(&board.runner, board.blocker, board.marauder),
        "CR 509.1b: blocking an unblockable attacker must be rejected"
    );
}

/// CR 118.12a + CR 701.21a: PAYING the cost prevents the effect — the defending
/// player sacrifices a creature of their choice, Ogre Marauder never gains
/// "can't be blocked", and a block with a surviving creature is legal.
#[test]
fn paying_the_sacrifice_keeps_ogre_marauder_blockable() {
    let mut board = attack_and_reach_payment_prompt();

    board
        .runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the defending player may choose to pay");
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::WardSacrificeChoice { .. }
        ),
        "CR 701.21a: paying must prompt the payer to choose which creature to sacrifice, got {:?}",
        board.runner.state().waiting_for
    );
    board
        .runner
        .act(GameAction::SelectCards {
            cards: vec![board.fodder],
        })
        .expect("the chosen creature pays the unless-cost");

    assert_ne!(
        board.runner.state().objects[&board.fodder].zone,
        Zone::Battlefield,
        "the sacrificed creature must leave the battlefield"
    );

    // CR 118.12a: the payment prevented the effect, so no grant was applied.
    assert!(
        !engine::game::combat::has_cant_be_blocked_static(board.runner.state(), board.marauder),
        "paying the unless-cost must prevent the \"can't be blocked\" grant"
    );
    assert!(
        block_is_legal(&board.runner, board.blocker, board.marauder),
        "CR 509.1a: having paid, the defending player may block normally"
    );

    // ... and the declaration goes through on the real action path.
    advance_to_declare_blockers(&mut board.runner);
    board
        .runner
        .declare_blockers(&[(board.blocker, board.marauder)])
        .expect("having paid, the defending player may block normally");
}

//! Regression for issue #2884 — Memory Plunder's free-cast "you may" prompt.
//!
//! Oracle: "You may cast target instant or sorcery card from an opponent's
//! graveyard without paying its mana cost." This parses to an *optional*
//! `Effect::CastFromZone { without_paying_mana_cost: true, target: instant/
//! sorcery owned by an opponent in the graveyard }`. When the controller accepts
//! the "you may cast" prompt, the targeted card must actually be cast — placed
//! on the stack — without paying its mana cost (CR 608.2g free-cast permission).
//!
//! The reported bug: accepting the prompt did NOTHING — the spell was never put
//! on the stack.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MEMORY_PLUNDER_ORACLE: &str = "You may cast target instant or sorcery card \
     from an opponent's graveyard without paying its mana cost.";

/// Build a scenario with Memory Plunder in P0's hand and a bolt-like instant in
/// the OPPONENT's (P1's) graveyard. Returns the runner plus the two object ids.
fn setup() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Fund P0's pool to cover Memory Plunder's {1} cost.
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
    );

    // A bolt-like instant sitting in the OPPONENT's graveyard (the legal target).
    let target = scenario
        .add_spell_to_graveyard(P1, "Graveyard Bolt", true)
        .from_oracle_text("Deal 3 damage to any target.")
        .with_mana_cost(ManaCost::generic(1))
        .id();

    // Memory Plunder in P0's hand with a {1} cost so the cast pipeline runs.
    let plunder = scenario
        .add_spell_to_hand_from_oracle(P0, "Memory Plunder", true, MEMORY_PLUNDER_ORACLE)
        .with_mana_cost(ManaCost::generic(1))
        .id();

    (scenario.build(), plunder, target)
}

/// Cast Memory Plunder from hand, target an instant in the OPPONENT's graveyard,
/// resolve it, accept the "you may cast" prompt, and assert the targeted card is
/// actually cast (lands on the stack). The reported bug left it inert.
#[test]
fn accepting_free_cast_prompt_puts_spell_on_stack() {
    let (mut runner, plunder, target) = setup();

    // Cast + commit Memory Plunder targeting the opponent-graveyard instant.
    let commit = runner.cast(plunder).target_object(target).commit();
    assert_eq!(
        commit.state().objects[&plunder].zone,
        Zone::Stack,
        "Memory Plunder must be on the stack before resolving"
    );

    // Resolve Memory Plunder. Drive to the optional prompt manually so we can
    // inspect the post-accept state.
    runner.resolve_top();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "expected an optional 'you may cast' prompt for P0; got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the free-cast prompt");

    // CR 608.2g + CR 601.2c: accepting drives the free cast DURING resolution.
    // Graveyard Bolt ("Deal 3 damage to any target") must reach its own
    // target-selection step — proof the cast is genuinely under way (the reported
    // bug left the prompt inert with the card still in the graveyard).
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { player, .. } if player == P0
        ),
        "the free cast must enter Graveyard Bolt's target selection; got {:?}",
        runner.state().waiting_for
    );

    // CR 601.2c → CR 601.2i: choose the bolt's target; the spell then lands on the
    // stack as the topmost object (CR 608.2g).
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("choose the free-cast spell's target");

    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Stack,
        "accepting the free-cast prompt must put the targeted spell on the stack; \
         zone = {:?}, waiting_for = {:?}",
        runner.state().objects[&target].zone,
        runner.state().waiting_for,
    );
}

/// Discriminator: DECLINING the prompt leaves the targeted card untouched in the
/// opponent's graveyard (no cast, no stack entry).
#[test]
fn declining_free_cast_prompt_leaves_card_in_graveyard() {
    let (mut runner, plunder, target) = setup();

    runner.cast(plunder).target_object(target).commit();
    runner.resolve_top();

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline the free-cast prompt");

    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Graveyard,
        "declining must leave the targeted spell in the opponent's graveyard"
    );
}

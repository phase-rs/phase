//! CR 107.1c + CR 701.21a (#4380): Slaughter the Strong — "Each player chooses
//! any number of creatures they control with total power 4 or less, then
//! sacrifices all other creatures they control."
//!
//! Drives the real cast/resolution pipeline and asserts:
//!   1. The caster is prompted to keep a subset (the bug sacrificed everything).
//!   2. A kept subset whose total power exceeds the cap is rejected.
//!   3. A valid kept subset survives; the rest are sacrificed.
//!   4. Every player with eligible creatures is prompted (CR 107.1c "any number"
//!      may be fewer/zero), and a chained tail waits for the keep choice.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SLAUGHTER: &str = "Each player chooses any number of creatures they control \
     with total power 4 or less, then sacrifices all other creatures they control.";

#[test]
fn slaughter_the_strong_keeps_chosen_subset_within_total_power() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // P0 (caster) controls three creatures totalling power 7 (> 4) — must choose.
    let a = scenario.add_creature(P0, "Big A", 3, 3).id();
    let b = scenario.add_creature(P0, "Big B", 3, 3).id();
    let c = scenario.add_creature(P0, "Small C", 1, 1).id();
    // P1 controls one power-2 creature (<= 4) — auto-keeps, sacrifices nothing.
    let d = scenario.add_creature(P1, "Opp D", 2, 2).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Slaughter the Strong", true, SLAUGHTER)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the free sorcery must succeed");
    runner.resolve_top();

    // The caster must choose which creatures (total power <= 4) to keep.
    match &runner.state().waiting_for {
        WaitingFor::KeepWithinTotalPowerChoice {
            player,
            cap,
            eligible,
            ..
        } => {
            assert_eq!(*player, P0, "the affected player makes their own choice");
            assert_eq!(*cap, 4);
            assert_eq!(
                eligible.len(),
                3,
                "all three of P0's creatures are eligible"
            );
        }
        other => panic!("expected KeepWithinTotalPowerChoice, got {other:?}"),
    }

    // Keeping A + B (power 6) exceeds the cap of 4 — rejected (state unchanged).
    assert!(
        runner
            .act(GameAction::ChooseKeptCreatures { kept: vec![a, b] })
            .is_err(),
        "a kept subset above the total-power cap must be rejected"
    );

    // Keep A (3) + C (1) = power 4 (within cap); B must be sacrificed.
    runner
        .act(GameAction::ChooseKeptCreatures { kept: vec![a, c] })
        .expect("keeping A + C (total power 4) is legal");

    // CR 107.1c: P1 is also prompted (the choice is "any number", which may be
    // fewer) even though its single power-2 creature already fits the cap.
    match &runner.state().waiting_for {
        WaitingFor::KeepWithinTotalPowerChoice {
            player, eligible, ..
        } => {
            assert_eq!(*player, P1, "P1 makes their own keep choice");
            assert_eq!(eligible, &vec![d], "P1's only creature is eligible");
        }
        other => panic!("expected P1's KeepWithinTotalPowerChoice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseKeptCreatures { kept: vec![d] })
        .expect("P1 keeps D");

    runner.advance_until_stack_empty();

    let alive = |id: ObjectId| {
        runner
            .state()
            .objects
            .get(&id)
            .is_some_and(|o| o.zone == Zone::Battlefield)
    };
    assert!(alive(a), "kept creature A survives");
    assert!(alive(c), "kept creature C survives");
    assert!(!alive(b), "unkept creature B is sacrificed");
    assert!(alive(d), "P1 kept D");
}

/// CR 107.1c: "any number" includes zero — a player may keep none of their
/// eligible creatures even when keeping all would already fit the cap, to
/// sacrifice their own creatures.
#[test]
fn slaughter_the_strong_player_may_keep_fewer_within_cap() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // A single power-1 creature — keeping it is trivially within the cap, yet the
    // controller must still be allowed to keep zero and sacrifice it.
    let lone = scenario.add_creature(P0, "Lone", 1, 1).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Slaughter the Strong", true, SLAUGHTER)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
    runner.resolve_top();

    // The controller is prompted even though keeping the lone creature fits the
    // cap — and chooses to keep nothing.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::KeepWithinTotalPowerChoice { .. }
        ),
        "an eligible creature within the cap must still prompt, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseKeptCreatures { kept: vec![] })
        .expect("keeping nothing is legal");
    runner.advance_until_stack_empty();

    assert!(
        runner
            .state()
            .objects
            .get(&lone)
            .is_none_or(|o| o.zone != Zone::Battlefield),
        "the voluntarily-unkept creature is sacrificed"
    );
}

/// CR 608.2 (#4600 review [MED]): a sub-ability/tail chained after the total-power
/// keep effect must wait for the player to answer the keep prompt — it must not
/// resolve while the game is paused on `KeepWithinTotalPowerChoice`.
const SLAUGHTER_THEN_GAIN: &str = "Each player chooses any number of creatures they \
     control with total power 4 or less, then sacrifices all other creatures they \
     control. You gain 2 life.";

#[test]
fn slaughter_the_strong_tail_waits_for_keep_choice() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // Two power-3 creatures (total 6 > 4) — the controller must choose a subset.
    let x = scenario.add_creature(P0, "X", 3, 3).id();
    let _y = scenario.add_creature(P0, "Y", 3, 3).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Slaughter Then Gain", true, SLAUGHTER_THEN_GAIN)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let spell_card = runner.state().objects[&spell].card_id;
    let life_before = runner.life(P0);
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: spell_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast");
    runner.resolve_top();

    // Paused on the keep choice — the "You gain 2 life" tail must NOT have run.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::KeepWithinTotalPowerChoice { .. }
        ),
        "resolution must pause on the keep choice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.life(P0),
        life_before,
        "the chained tail must wait for the keep choice"
    );

    runner
        .act(GameAction::ChooseKeptCreatures { kept: vec![x] })
        .expect("keep X");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        life_before + 2,
        "the tail resolves only after the keep choice is answered"
    );
}

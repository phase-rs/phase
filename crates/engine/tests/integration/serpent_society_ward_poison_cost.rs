//! Regression for issue #6640: The Serpent Society's Ward—Get five poison
//! counters never gave the targeting opponent poison counters, because the
//! Oracle parser had no `WardCost` variant for "give yourself N counters" and
//! silently fell back to `WardCost::Mana(generic: 0)` — a free, always-paid
//! Ward that does nothing.
//!
//! https://github.com/phase-rs/phase/issues/6640
//!
//! CR references:
//!   - CR 702.21a: Ward — counter the targeting spell/ability unless the
//!     targeting player pays the stated cost.
//!   - CR 122.1 + CR 104.3d: giving a player poison counters; a player with
//!     ten or more poison counters loses the game (a separate SBA, not
//!     exercised by this test).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerCounterKind;

const SERPENT_SOCIETY: &str = "Deathtouch\n\
Ward—Get five poison counters. (A player with ten or more poison counters loses the game.)\n\
Whenever another creature you control with deathtouch dies, each opponent sacrifices a nontoken creature of their choice.";

#[test]
fn serpent_society_ward_prompts_the_targeting_opponent_for_poison_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let serpent_society = scenario
        .add_creature_from_oracle(P0, "The Serpent Society", 3, 4, SERPENT_SOCIETY)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy Spell", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner
        .cast(destroy)
        .target_objects(&[serpent_society])
        .commit();
    runner.advance_until_stack_empty();

    let WaitingFor::UnlessPayment { player, cost, .. } = &runner.state().waiting_for else {
        panic!(
            "Ward must prompt the targeting opponent to pay the poison-counter cost, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1);
    assert!(matches!(
        cost,
        engine::types::ability::AbilityCost::GetPlayerCounters {
            counter_kind: PlayerCounterKind::Poison,
            count: 5,
        }
    ));
}

#[test]
fn serpent_society_ward_declined_counters_the_spell_and_gives_no_poison() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let serpent_society = scenario
        .add_creature_from_oracle(P0, "The Serpent Society", 3, 4, SERPENT_SOCIETY)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy Spell", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner
        .cast(destroy)
        .target_objects(&[serpent_society])
        .commit();
    runner.advance_until_stack_empty();

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining Ward must be a legal action");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        0,
        "declining Ward's cost must not give the opponent any poison counters"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&serpent_society)
            .is_some_and(|obj| obj.zone == engine::types::zones::Zone::Battlefield),
        "declining Ward's cost must counter the targeting spell, leaving Serpent Society alive"
    );
    assert!(
        !runner.state().stack.iter().any(|entry| entry.id == destroy),
        "the countered spell must be removed from the stack"
    );
}

#[test]
fn serpent_society_ward_paid_gives_five_poison_and_the_spell_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let serpent_society = scenario
        .add_creature_from_oracle(P0, "The Serpent Society", 3, 4, SERPENT_SOCIETY)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy Spell", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner
        .cast(destroy)
        .target_objects(&[serpent_society])
        .commit();
    runner.advance_until_stack_empty();

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the opponent pays Ward's poison-counter cost");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        5,
        "paying Ward's cost must give the targeting opponent five poison counters"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&serpent_society)
            .is_none_or(|obj| obj.zone != engine::types::zones::Zone::Battlefield),
        "paying Ward's cost must let the targeted destroy spell resolve, removing Serpent Society from the battlefield"
    );
}

/// CR 104.3d + CR 704.5c: a payment that pushes the payer to ten or more
/// poison counters must trigger the loss state-based action immediately —
/// before the targeted destroy spell gets a chance to continue resolving.
/// Mirrors `crates/engine/src/game/sba.rs`'s own `sba_poison_10_player_loses`
/// unit test's expected shape.
#[test]
fn serpent_society_ward_payment_that_reaches_ten_poison_loses_the_game() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let serpent_society = scenario
        .add_creature_from_oracle(P0, "The Serpent Society", 3, 4, SERPENT_SOCIETY)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy Spell", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
        state.players[P1.0 as usize].poison_counters = 5;
    }

    runner
        .cast(destroy)
        .target_objects(&[serpent_society])
        .commit();
    runner.advance_until_stack_empty();

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the opponent pays Ward's poison-counter cost");

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        10,
        "5 existing + 5 from Ward's cost must reach the ten-poison threshold"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(p) } if p == P0
        ),
        "reaching ten poison must trigger the CR 104.3d loss SBA immediately, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner
            .state()
            .objects
            .get(&serpent_society)
            .is_some_and(|obj| obj.zone == engine::types::zones::Zone::Battlefield),
        "the game must end (P1 loses) before the destroy spell gets a chance to resolve, so Serpent Society must still be on the battlefield"
    );
}

/// CR 122.1 + CR 614.17 + CR 702.21a: Solemnity's "Players can't get
/// counters" replacement must make Ward's poison-counter cost a FAILED
/// payment, not a free bypass. Before this fix, `add_player_counter_with_
/// replacement` reported `Prevented` as if it were a paid cost, so the
/// targeting opponent's spell would incorrectly continue resolving even
/// though no poison was actually given — nullifying Ward's entire deterrent
/// for free. Solemnity's real Oracle text is "Players can't get counters.
/// Prevent all damage that would be dealt to permanents by sources with
/// counters on them." — only the first (relevant) sentence is used here.
#[test]
fn serpent_society_ward_payment_prevented_by_solemnity_counters_the_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Solemnity", 0, 0, "Players can't get counters.")
        .as_enchantment();
    let serpent_society = scenario
        .add_creature_from_oracle(P0, "The Serpent Society", 3, 4, SERPENT_SOCIETY)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy Spell", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner
        .cast(destroy)
        .target_objects(&[serpent_society])
        .commit();
    runner.advance_until_stack_empty();

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("attempting to pay Ward's poison-counter cost must be a legal action even when Solemnity prevents the actual counter gain");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        0,
        "Solemnity must prevent the poison counters from actually being given"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&serpent_society)
            .is_some_and(|obj| obj.zone == engine::types::zones::Zone::Battlefield),
        "a prevented player-counter payment must be treated as a FAILED cost, countering the targeting spell exactly like a declined payment — Serpent Society must survive"
    );
    assert!(
        !runner.state().stack.iter().any(|entry| entry.id == destroy),
        "the countered spell must be removed from the stack"
    );
}

//! Building-block coverage for issue #6431: under Emrakul-style turn control
//! (CR 723), a flashback cost that sacrifices a permanent must be paid from
//! the semantic caster's own permanents — the controlled player's, not the
//! controller's.
//!
//! Lava Dart: "Flashback—Sacrifice a Mountain." (CR 702.34a additional cost,
//! CR 601.2h). While P0 controls P1's turn via an Emrakul-style effect (CR
//! 723.5), P1's own Lava Dart sits in P1's graveyard and P1 controls a
//! Mountain. P0, authorized to make P1's choices, must be able to pay the
//! flashback cost by sacrificing P1's Mountain.
//!
//! The engine-side cost/target plumbing this test exercises (`player`
//! threaded through `handle_cast_spell_with_payment_mode` →
//! `pay_additional_cost` → `find_eligible_sacrifice_targets`) was already
//! correct on `main`. The reported defect turned out to live in the frontend
//! dispatch queue (`client/src/game/dispatch.ts`'s `waitingForActorMatches`),
//! which dropped the queued sacrifice-cost response as "stale" under turn
//! control — see `client/src/game/__tests__/dispatchTurnControlPayCostQueue.test.ts`
//! for that regression. This test stays as building-block coverage pinning
//! the engine's half of the contract.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::{CastingVariant, WaitingFor};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

#[test]
fn lava_dart_flashback_sacrifice_payable_under_turn_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mountain = scenario.add_basic_land(P1, ManaColor::Red);
    let lava_dart = scenario
        .add_spell_to_graveyard(P1, "Lava Dart", true)
        .from_oracle_text(
            "Lava Dart deals 1 damage to any target.\n\
             Flashback\u{2014}Sacrifice a Mountain. (You may cast this card from your \
             graveyard for its flashback cost. Then exile it.)",
        )
        .id();
    let mut runner = scenario.build();

    {
        let state = runner.state_mut();
        // CR 723: P0 controls P1's turn.
        state.active_player = P1;
        state.turn_decision_controller = Some(P0);
        // P1 holds priority; sync re-derives the authorized submitter (P0).
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P1 });
    }

    let outcome = runner
        .cast(lava_dart)
        .casting_variant(CastingVariant::Flashback)
        .target_player(P0)
        .sacrifice_with(&[mountain])
        .try_resolve();

    let outcome = outcome.unwrap_or_else(|err| {
        panic!(
            "P0 (controlling P1's turn) must be able to pay Lava Dart's flashback \
             Sacrifice-a-Mountain cost with P1's own Mountain, got: {err:?}"
        )
    });

    assert_eq!(
        outcome.zone_of(mountain),
        Zone::Graveyard,
        "the sacrificed Mountain must have moved to the graveyard"
    );
    assert!(
        outcome.state().exile.contains(&lava_dart),
        "CR 702.34a: a spell cast via flashback is exiled instead of going anywhere else \
         when it leaves the stack"
    );
    assert_eq!(
        outcome.life_delta(P0),
        -1,
        "Lava Dart must deal 1 damage to its target"
    );
}

//! Repro for the multiplayer "each opponent sacrifices a creature" bug
//! (Sheoldred's Edict / Momentum Breaker). In a 4-player game each opponent
//! that controls more eligible permanents than the sacrifice count must pause
//! on its own `EffectZoneChoice`; the `player_scope` driver stashes a
//! `pending_continuation` for the remaining opponents, which must resume after
//! each choice resolves.
//!
//! CR 701.21a: each affected player sacrifices a permanent THEY control.
//! CR 608.2e: the scoped instruction is performed once per affected player.

use engine::game::scenario::GameScenario;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

const SHEOLDRED_EDICT: &str = "Choose one —\n\
• Each opponent sacrifices a nontoken creature of their choice.\n\
• Each opponent sacrifices a creature token of their choice.\n\
• Each opponent sacrifices a planeswalker of their choice.";

#[test]
fn each_opponent_sacrifices_a_creature_in_four_player() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Each of the three opponents controls TWO nontoken creatures, so each must
    // make a choice (the eligible pool exceeds the count of 1) — this is the
    // path that round-trips through EffectZoneChoice + pending_continuation.
    for opp in [P1, P2, P3] {
        scenario.add_vanilla(opp, 2, 2);
        scenario.add_vanilla(opp, 3, 3);
    }

    // A Blood Artist-style death observer (the kind a real Tergrid/Muldrotha
    // sacrifice deck runs). Each opponent's sacrifice fires this trigger; if the
    // observer-trigger handling clobbers the pending EffectZoneChoice of the
    // *next* opponent, only the first opponent would end up sacrificing.
    scenario.add_creature_from_oracle(
        P0,
        "Blood Artist",
        0,
        1,
        "Whenever a creature dies, you gain 1 life.",
    );

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Sheoldred's Edict", true, SHEOLDRED_EDICT)
        .id();

    let mut runner = scenario.build();
    // Fund {1}{B}.
    for _ in 0..2 {
        let unit = engine::types::mana::ManaUnit::new(
            ManaType::Black,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        );
        runner.state_mut().players[0].mana_pool.add(unit);
    }

    // Mode 0: "Each opponent sacrifices a nontoken creature of their choice."
    let outcome = runner.cast(spell).modes(&[0]).resolve();

    // First opponent (APNAP after P0) must be prompted to choose.
    let mut waiting = outcome.final_waiting_for().clone();
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 10, "too many prompts; stuck at {waiting:?}");
        match waiting {
            WaitingFor::EffectZoneChoice {
                player, ref cards, ..
            } => {
                let pick = cards[0];
                let res = runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .unwrap_or_else(|e| panic!("opponent {player:?} choice failed: {e:?}"));
                waiting = res.waiting_for;
            }
            _ => break,
        }
    }

    // Each opponent must have lost exactly one creature.
    for opp in [P1, P2, P3] {
        assert_eq!(
            runner.battlefield_count(opp),
            1,
            "opponent {opp:?} must sacrifice exactly one creature (had 2)"
        );
    }
}

/// Momentum Breaker's ETB effect shape: an `Or` filter ("creature or Vehicle")
/// PLUS a conditional `Discard` rider ("each opponent who can't discards a
/// card"). Real-game log showed only the FIRST opponent in turn order
/// sacrificing — the other three never acted. Reproduce as a cast spell with
/// the identical Oracle text.
const MOMENTUM_BREAKER_EFFECT: &str =
    "Each opponent sacrifices a creature or Vehicle of their choice. \
Each opponent who can't discards a card.";

#[test]
fn momentum_breaker_each_opponent_sacrifices_in_four_player() {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Each opponent controls TWO creatures (eligible for "creature or Vehicle")
    // so each must choose, and a card in hand (so the "who can't discards"
    // rider has something to act on if the sacrifice path were skipped).
    for opp in [P1, P2, P3] {
        scenario.add_vanilla(opp, 2, 2);
        scenario.add_vanilla(opp, 3, 3);
        scenario.with_cards_in_hand(opp, &["Filler"]);
    }

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Momentum Breaker Effect", true, MOMENTUM_BREAKER_EFFECT)
        .id();

    let mut runner = scenario.build();
    for _ in 0..4 {
        let unit = engine::types::mana::ManaUnit::new(
            ManaType::Black,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        );
        runner.state_mut().players[0].mana_pool.add(unit);
    }

    let outcome = runner.cast(spell).resolve();

    let mut waiting = outcome.final_waiting_for().clone();
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 12, "too many prompts; stuck at {waiting:?}");
        match waiting {
            WaitingFor::EffectZoneChoice {
                player, ref cards, ..
            } => {
                let pick = cards[0];
                let res = runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .unwrap_or_else(|e| panic!("opponent {player:?} sac choice failed: {e:?}"));
                waiting = res.waiting_for;
            }
            WaitingFor::DiscardChoice {
                player, ref cards, ..
            } => {
                let pick = cards[0];
                let res = runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .unwrap_or_else(|e| panic!("opponent {player:?} discard failed: {e:?}"));
                waiting = res.waiting_for;
            }
            _ => break,
        }
    }

    for opp in [P1, P2, P3] {
        assert_eq!(
            runner.battlefield_count(opp),
            1,
            "opponent {opp:?} must sacrifice exactly one creature (had 2)"
        );
    }
}

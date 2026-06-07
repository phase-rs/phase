//! Regression: an Overload spell (CR 702.96a) must be castable even when the
//! PRINTED cast has no legal target, because the overload mode requires none
//! (CR 702.96b: "that spell won't require any targets. It may affect objects
//! that couldn't be chosen as legal targets...").
//!
//! Before the fix, `can_cast_prepared_now` gated targets against the UNMODIFIED
//! printed object ("deals 4 damage to target creature you don't control"), so
//! Mizzium Mortars vs an empty opposing board was filtered out of legal actions
//! entirely — even though the target-less overload mode (DealDamage → DamageAll)
//! is perfectly legal. The gate now evaluates the TRANSFORMED `ability_def`.
//!
//! Real cards ("Mizzium Mortars", "Cyclonic Rift") are referenced as string
//! literals so `gen-test-fixture.py` auto-extracts them into the integration
//! fixture. The tests self-skip when the shared card DB is unavailable (CI).

use engine::ai_support::legal_actions;
use engine::game::rehydrate_game_from_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::game_state::{AlternativeCastKeyword, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn add_pool(runner: &mut GameRunner, player: PlayerId, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

/// True when `legal_actions` offers a `CastSpell` for `object_id`.
fn castable(runner: &GameRunner, object_id: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|a| matches!(a, GameAction::CastSpell { object_id: o, .. } if *o == object_id))
}

/// Drive the cast through the overload choice, passing priority until the stack
/// settles so the overloaded effect (DamageAll / BounceAll) resolves.
fn cast_overload_and_resolve(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
        })
        .expect("cast overload spell");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AlternativeCastChoice {
                keyword: AlternativeCastKeyword::Overload,
                ..
            }
        ),
        "expected AlternativeCastChoice(Overload), got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("opt into overload");

    // Settle the stack — the overload mode requires no target selection.
    for _ in 0..40 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

#[test]
fn mizzium_mortars_overload_castable_with_no_opposing_creature() {
    let Some(db) = load_db() else {
        eprintln!("skipping: integration card fixture not available");
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mizzium = scenario.add_real_card(P0, "Mizzium Mortars", Zone::Hand, db);
    // P0's own creature — the only creature on the board. The printed cast
    // ("target creature you don't control") has NO legal target.
    let own_creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    // Overload cost {3}{R}{R}{R}.
    add_pool(
        &mut runner,
        P0,
        &[
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
        ],
    );

    // CR 702.96b: castable despite no legal PRINTED target (FAILS on origin/main).
    assert!(
        castable(&runner, mizzium),
        "overload Mizzium Mortars must be a legal action with no opposing creature"
    );

    cast_overload_and_resolve(&mut runner, mizzium);

    // The overloaded spell resolved and left the stack for the graveyard.
    assert!(
        runner
            .state()
            .players
            .iter()
            .any(|p| p.graveyard.contains(&mizzium)),
        "overloaded Mizzium Mortars must resolve and go to the graveyard"
    );
    // CR 702.96b: the overloaded text is "deal 4 damage to EACH creature you
    // don't control" — the caster's own creature is NOT affected (it is not a
    // creature the caster doesn't control).
    assert_eq!(
        runner.state().objects[&own_creature].damage_marked,
        0,
        "overload DamageAll (\"each you don't control\") must spare the caster's own creature"
    );
}

#[test]
fn mizzium_mortars_offers_both_modes_when_printed_target_legal() {
    let Some(db) = load_db() else {
        eprintln!("skipping: integration card fixture not available");
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mizzium = scenario.add_real_card(P0, "Mizzium Mortars", Zone::Hand, db);
    // Opponent controls a creature — the printed target IS legal now.
    let _opp_creature = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    add_pool(
        &mut runner,
        P0,
        &[
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
            ManaType::Red,
        ],
    );

    let card_id = runner.state().objects[&mizzium].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: mizzium,
            card_id,
            targets: vec![],
        })
        .expect("cast Mizzium Mortars");

    // Non-regression: both printed and overload modes are affordable, so the
    // engine prompts the alternative-cast choice rather than forcing overload.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AlternativeCastChoice {
                keyword: AlternativeCastKeyword::Overload,
                ..
            }
        ),
        "expected AlternativeCastChoice(Overload) when both modes legal, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn cyclonic_rift_overload_castable_with_no_opposing_permanent() {
    let Some(db) = load_db() else {
        eprintln!("skipping: integration card fixture not available");
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rift = scenario.add_real_card(P0, "Cyclonic Rift", Zone::Hand, db);
    // P0's own creature — P1 controls no nonland permanent, so the printed cast
    // ("target nonland permanent you don't control") has NO legal target.
    let own_creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    // Overload cost {6}{U}.
    add_pool(
        &mut runner,
        P0,
        &[
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Blue,
        ],
    );

    // CR 702.96b: castable despite no legal PRINTED target (FAILS on origin/main).
    assert!(
        castable(&runner, rift),
        "overload Cyclonic Rift must be a legal action with no opposing nonland permanent"
    );

    cast_overload_and_resolve(&mut runner, rift);

    // The overloaded spell resolved and left the stack for the graveyard.
    assert!(
        runner
            .state()
            .players
            .iter()
            .any(|p| p.graveyard.contains(&rift)),
        "overloaded Cyclonic Rift must resolve and go to the graveyard"
    );
    // CR 702.96b: the overloaded text is "return EACH nonland permanent you
    // don't control" — the caster's own creature is NOT bounced (it is not a
    // permanent the caster doesn't control); it stays on the battlefield.
    assert_eq!(
        runner.state().objects[&own_creature].zone,
        Zone::Battlefield,
        "overload BounceAll (\"each you don't control\") must spare the caster's own creature"
    );
}

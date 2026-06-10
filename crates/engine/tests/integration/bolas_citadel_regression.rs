//! Regression: GitHub issue #946 — Bolas's Citadel casting blocked despite
//! tapping the correct mana.
//!
//! Covers two surfaces:
//! 1. Casting the Citadel artifact from hand at {3}{B}{B}{B}.
//! 2. Casting a spell from the top of the library via Citadel's permission
//!    (pay life equal to mana value, not mana).

use engine::game::casting::{
    can_cast_object_now, display_spell_cost, effective_spell_cost, spell_objects_available_to_cast,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;

const BOLAS_ORACLE: &str = "You may look at the top card of your library any time.\n\
You may play lands and cast spells from the top of your library. If you cast a spell this way, pay life equal to its mana value rather than pay its mana cost.\n\
{T}, Sacrifice ten nonland permanents: Each opponent loses 10 life.";

fn citadel_mana_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![
            ManaCostShard::Black,
            ManaCostShard::Black,
            ManaCostShard::Black,
        ],
        generic: 3,
    }
}

fn pool_units(colors: &[ManaType]) -> Vec<ManaUnit> {
    let dummy = engine::types::identifiers::ObjectId(0);
    colors
        .iter()
        .map(|&color| ManaUnit::new(color, dummy, false, vec![]))
        .collect()
}

/// Issue #946: the Citadel itself must be castable from hand when the pool can
/// pay {3}{B}{B}{B} (the common "I tapped the specific black mana" report).
#[test]
fn bolas_citadel_castable_from_hand_with_exact_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let citadel_id = scenario
        .add_creature_to_hand(P0, "Bolas's Citadel", 0, 0)
        .as_artifact()
        .with_mana_cost(citadel_mana_cost())
        .from_oracle_text(BOLAS_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        pool_units(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
            ManaType::Black,
            ManaType::Black,
        ]),
    );

    let runner = scenario.build();
    assert!(
        can_cast_object_now(runner.state(), P0, citadel_id),
        "Bolas's Citadel must be castable from hand when 3BBB is in the pool"
    );
    assert!(
        engine::ai_support::legal_actions(runner.state())
            .iter()
            .any(
                |a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == citadel_id)
            ),
        "legal_actions must expose CastSpell for Citadel in hand"
    );
}

/// Citadel on the battlefield must parse the top-of-library permission with a
/// PayLife alt-cost rider so library casts replace mana with life.
#[test]
fn bolas_citadel_static_carries_library_alt_cost() {
    let mut scenario = GameScenario::new();
    let citadel_id = scenario
        .add_creature(P0, "Bolas's Citadel", 0, 0)
        .as_artifact()
        .from_oracle_text(BOLAS_ORACLE)
        .id();
    let runner = scenario.build();
    let obj = runner.state().objects.get(&citadel_id).unwrap();
    let top_perm = obj
        .static_definitions
        .iter_unchecked()
        .find(|d| matches!(d.mode, StaticMode::TopOfLibraryCastPermission { .. }))
        .expect("TopOfLibraryCastPermission static");
    match &top_perm.mode {
        StaticMode::TopOfLibraryCastPermission { alt_cost, .. } => {
            assert!(
                alt_cost.is_some(),
                "Bolas's Citadel must attach PayLife alt_cost to the library permission"
            );
        }
        other => panic!("unexpected static mode: {other:?}"),
    }
}

/// Issue #946 sibling: with Citadel in play, a bolt on top of library must be
/// castable with life only — prepared mana payment is skipped.
#[test]
fn bolas_citadel_library_top_skips_mana_and_surfaces_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _citadel = scenario
        .add_creature(P0, "Bolas's Citadel", 0, 0)
        .as_artifact()
        .from_oracle_text(BOLAS_ORACLE)
        .id();
    let bolt_id = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text("Lightning Bolt deals 3 damage to any target.")
        .id();
    scenario.with_life(P0, 20);

    let runner = scenario.build();
    let effective = effective_spell_cost(runner.state(), P0, bolt_id).expect("effective cost");
    assert!(
        effective.is_without_paying_mana(),
        "library cast via Citadel must skip mana payment"
    );
    let display = display_spell_cost(runner.state(), P0, bolt_id).expect("display cost");
    assert!(
        display.is_without_paying_mana(),
        "UI cost display must not show the printed mana cost for Citadel casts"
    );

    let available = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        available.contains(&bolt_id),
        "Citadel must surface the library top as castable"
    );
    assert!(
        can_cast_object_now(runner.state(), P0, bolt_id),
        "library-top bolt must pass can_cast when life is available"
    );
}

/// PayLife affordability must gate library-top casts — not just mana.
#[test]
fn bolas_citadel_library_top_not_castable_without_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _citadel = scenario
        .add_creature(P0, "Bolas's Citadel", 0, 0)
        .as_artifact()
        .from_oracle_text(BOLAS_ORACLE)
        .id();
    let bolt_id = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text("Lightning Bolt deals 3 damage to any target.")
        .id();
    scenario
        .with_life(P0, 0)
        .with_mana_pool(P0, pool_units(&[ManaType::Red]));

    let runner = scenario.build();
    assert!(
        !can_cast_object_now(runner.state(), P0, bolt_id),
        "library-top cast must be uncastable at 0 life even with mana in pool"
    );
}

/// End-to-end: casting from the library top via Citadel must route through
/// target selection, not a nonzero mana-payment step.
#[test]
fn bolas_citadel_library_top_cast_enters_targeting_not_mana_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let _citadel = scenario
        .add_creature(P0, "Bolas's Citadel", 0, 0)
        .as_artifact()
        .from_oracle_text(BOLAS_ORACLE)
        .id();
    let bolt_id = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text("Lightning Bolt deals 3 damage to any target.")
        .id();
    let target = scenario.add_creature(P1, "Target", 2, 2).id();

    let mut runner: GameRunner = scenario.build();
    let card_id = runner.state().objects[&bolt_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt_id,
            card_id,
            targets: vec![target],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell from library top should start");

    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { .. } | WaitingFor::OptionalCostChoice { .. } => {}
        WaitingFor::ManaPayment { .. } => {
            panic!("Citadel library cast must not enter ManaPayment for a zeroed mana cost")
        }
        other => panic!("unexpected waiting state after starting library-top cast: {other:?}"),
    }
}

//! Regression guards for issue #1509: "can't cast a sorcery on main phase"
//! despite mana sources being available.
//!
//! The Discord report had no card names (`status:needs-repro`). Two engine
//! classes that produce this symptom are covered here:
//!   1. Baseline sorcery-speed + reachable mana must surface `CastSpell` in
//!      `legal_actions` during an empty-stack main phase.
//!   2. `TapsForMana` triggered mana (Leyline of Abundance / aura class) must
//!      count toward auto-tapped cast affordability so the spell is not silently
//!      dropped from `legal_actions` (see also `leyline_taps_for_mana_repro`).

use engine::ai_support::legal_actions;
use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const LEYLINE_TEXT: &str = "Whenever you tap a creature for mana, add an additional {G}.";

fn cost_3gg() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Green, ManaCostShard::Green],
        generic: 3,
    }
}

fn floating_green(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
        .collect()
}

/// CR 307.1 + CR 117.1a: sorcery in hand, active player's empty-stack main phase,
/// enough untapped Forests — must be offered as a legal cast.
#[test]
fn sorcery_with_untapped_lands_appears_in_legal_actions() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..5 {
        scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    }
    let spell_id = scenario
        .add_spell_to_hand(P0, "Test Sorcery", false)
        .with_mana_cost(cost_3gg())
        .id();

    let runner = scenario.build();
    let state = runner.state();
    assert!(
        can_cast_object_now(state, P0, spell_id),
        "can_cast_object_now must be true for a {{3}}{{G}}{{G}} sorcery with five untapped Forests"
    );
    let actions = legal_actions(state);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::CastSpell { object_id, .. } if *object_id == spell_id
        )),
        "legal_actions must include CastSpell for the sorcery (issue #1509 baseline)"
    );
}

/// CR 605.4a: TapsForMana bonus mana must reach the castability gate, not only
/// the affordability preview — otherwise the sorcery vanishes from legal_actions.
#[test]
fn taps_for_mana_bonus_keeps_sorcery_castable_in_legal_actions() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Mana Beast", 1, 1, "{T}: Add {G}.");
    scenario.add_creature_from_oracle(P0, "Abundance Source", 0, 1, LEYLINE_TEXT);
    scenario.with_mana_pool(P0, floating_green(3));
    let spell_id = scenario
        .add_spell_to_hand(P0, "Big Sorcery", false)
        .with_mana_cost(cost_3gg())
        .id();

    let runner = scenario.build();
    let state = runner.state();
    assert!(
        can_cast_object_now(state, P0, spell_id),
        "can_cast_object_now must count Leyline-class TapsForMana bonus mana"
    );
    let actions = legal_actions(state);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::CastSpell { object_id, .. } if *object_id == spell_id
        )),
        "legal_actions must include CastSpell when 3 floating + tap + Leyline = 5 mana (issue #1509)"
    );
}

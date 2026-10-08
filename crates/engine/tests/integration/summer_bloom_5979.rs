//! Regression test for issue #5979 — Summer Bloom's "You may play up to three
//! additional lands this turn." must not only parse to the `AdditionalLandDrop`
//! grant but actually reach `additional_land_drops` through cast + stack
//! resolution (the pre-fix parser misrouted it to `CastFromZone`, so no
//! land-play allowance was ever registered).
//!
//! Mirrors the runtime pattern in `explore_spell_3315.rs`: cast the real Oracle
//! text via `GameScenario` + `GameRunner::cast(...).resolve()` and observe the
//! transient continuous effect the production resolver registers.

use engine::game::scenario::{GameScenario, P0};
use engine::game::static_abilities::additional_land_drops;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

/// CR 305.2: "play up to three additional lands this turn" grants the same +3
/// land-play allowance as a bare "three additional lands" — the "up to" hedge is
/// redundant because land plays are already optional.
#[test]
fn summer_bloom_grants_three_additional_land_drops_at_runtime() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Summer Bloom",
            false, // sorcery
            "You may play up to three additional lands this turn.",
        )
        .with_mana_cost(ManaCost::zero())
        .id();

    let outcome = scenario.build().cast(spell).resolve();

    assert_eq!(
        additional_land_drops(outcome.state(), P0),
        3,
        "Summer Bloom must grant 3 additional land drops after resolving"
    );
}

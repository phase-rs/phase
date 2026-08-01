//! Regression for issue #6332: the Legends (1994) banding-land cycle —
//! "<Color> legendary creatures you control have \"bands with other legendary
//! creatures.\"" (Unholy Citadel [Black], Seafarer's Quay [Blue], Adventurers'
//! Guildhouse [Green], Cathedral of Serra [White], Mountain Stronghold [Red]) —
//! failed to parse because its subject compounds a color adjective with the
//! legendary supertype, a descriptor shape `parse_typed_you_control`
//! (`oracle_static/anthem.rs`) didn't recognize. The card did nothing.
//!
//! This test drives the fix through the real pipeline — Oracle text on a
//! battlefield land, layer evaluation, then reading the granted keyword off
//! each creature — rather than hand-constructing the expected AST, so it pins
//! the parser fix AND its composition with the pre-existing keyword-grant and
//! quality-normalization machinery (`parse_granted_keyword_fragment`,
//! `normalize_bands_with_other_quality`) end to end.
//!
//! https://github.com/phase-rs/phase/issues/6332

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard};

const UNHOLY_CITADEL_ORACLE: &str = "Black legendary creatures you control have \"bands with other legendary creatures.\" (Any legendary creatures can attack in a band as long as at least one has \"bands with other legendary creatures.\" Bands are blocked as a group. If at least two legendary creatures you control, one of which has \"bands with other legendary creatures,\" are blocking or being blocked by the same creature, you divide that creature's combat damage, not its controller, among any of the creatures it's being blocked by or is blocking.)";

fn black_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black],
        generic: 1,
    }
}

fn blue_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 1,
    }
}

#[test]
fn unholy_citadel_grants_bands_with_other_only_to_black_legendary_creatures() {
    let mut scenario = GameScenario::new();

    scenario.add_land_from_oracle(P0, "Unholy Citadel", UNHOLY_CITADEL_ORACLE);

    // Matches: Black AND legendary.
    let black_legend = scenario
        .add_creature(P0, "Test Black Legend", 2, 2)
        .as_legendary()
        .with_mana_cost(black_cost())
        .id();
    // Wrong color: legendary, but Blue not Black.
    let blue_legend = scenario
        .add_creature(P0, "Test Blue Legend", 2, 2)
        .as_legendary()
        .with_mana_cost(blue_cost())
        .id();
    // Wrong supertype: Black, but not legendary.
    let black_nonlegend = scenario
        .add_creature(P0, "Test Black Nonlegend", 2, 2)
        .with_mana_cost(black_cost())
        .id();
    // Correct color and supertype, but wrong controller.
    let opponent_black_legend = scenario
        .add_creature(P1, "Opponent Black Legend", 2, 2)
        .as_legendary()
        .with_mana_cost(black_cost())
        .id();

    let mut runner = scenario.build();
    // Force a full layer re-evaluation so the land's static ability is applied
    // before inspecting the battlefield's derived keywords.
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let state = runner.state();
    let granted = Keyword::BandsWithOther("Legend".to_string());

    assert!(
        state.objects[&black_legend].keywords.contains(&granted),
        "a Black legendary creature you control must receive the granted keyword: {:?}",
        state.objects[&black_legend].keywords
    );
    assert!(
        !state.objects[&blue_legend].keywords.contains(&granted),
        "a Blue legendary creature must NOT receive a Black-scoped grant: {:?}",
        state.objects[&blue_legend].keywords
    );
    assert!(
        !state.objects[&black_nonlegend].keywords.contains(&granted),
        "a non-legendary Black creature must NOT receive the grant: {:?}",
        state.objects[&black_nonlegend].keywords
    );
    assert!(
        !state.objects[&opponent_black_legend]
            .keywords
            .contains(&granted),
        "an opponent's Black legendary creature must NOT receive a you-control grant: {:?}",
        state.objects[&opponent_black_legend].keywords
    );
}

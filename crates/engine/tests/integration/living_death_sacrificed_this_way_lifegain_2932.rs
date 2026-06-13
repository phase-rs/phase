//! Living Death (#2932) — "sacrificed this way" must read the SACRIFICED set,
//! not the merged exile+sacrifice chain set.
//!
//! Oracle (regression shape the maintainer flagged):
//!   "Exile all creature cards from your graveyard, then sacrifice all creatures
//!    you control, then you gain life equal to the number of creatures
//!    sacrificed this way."
//!
//! CR 608.2c / CR 400.7: the spell resolves in three ordered steps — (1) exile
//! the graveyard creature cards (those land in EXILE), (2) sacrifice the
//! battlefield creatures (those land in the GRAVEYARD), (3) gain life equal to
//! the number of creatures SACRIFICED this way. Steps 1 and 2 publish into the
//! same chain tracked set, but the life-gain in step 3 names only the SACRIFICE
//! verb, so it must count only the members that landed in the graveyard.
//!
//! Maintainer's exact regression case: 2 graveyard creatures + 1 battlefield
//! creature → life gain MUST equal 1 (the single sacrificed creature), NOT 2
//! (the exiled cards) and NOT 3 (the merged set). The old PR's producer-kind
//! suppression failed this case; binding the "sacrificed this way" consumer to
//! its destination zone (Graveyard) via the verb the parser already sees makes
//! it read exactly the sacrificed subset.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::AbilityKind;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;

const ORACLE: &str = "Exile all creature cards from your graveyard, then sacrifice all creatures you control, then you gain life equal to the number of creatures sacrificed this way.";

/// CR 608.2c + CR 400.7: the "sacrificed this way" life-gain binds to the
/// GRAVEYARD landing zone, so it counts only the creature sacrificed in step 2,
/// never the two creature cards exiled in step 1 that share the chain set.
#[test]
fn living_death_sacrificed_this_way_lifegain_counts_only_sacrificed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two creature CARDS in the graveyard → EXILED this way (land in Exile).
    scenario.add_creature_to_graveyard(P0, "Walking Corpse", 2, 2);
    scenario.add_creature_to_graveyard(P0, "Bone Sentry", 2, 2);

    // One battlefield creature → SACRIFICED this way (lands in Graveyard).
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);

    // Non-creature source so the spell source never matches the creature filter.
    let source = scenario.add_basic_land(P0, ManaColor::Black);

    let mut runner = scenario.build();
    let starting_life = runner.state().players[0].life;

    let def = parse_effect_chain(ORACLE, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Living Death lifegain variant must resolve");

    // Exactly the one sacrificed creature counts — NOT the two exiled cards.
    assert_eq!(
        runner.state().players[0].life,
        starting_life + 1,
        "life gain must equal the single creature sacrificed this way (Graveyard members), \
         not the two creature cards exiled this way (Exile members) in the same chain set"
    );
}

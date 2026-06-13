//! Living Death (#2932) — SAME-DESTINATION disambiguation. The maintainer's
//! first [HIGH] objection: distinct producer actions that share a destination
//! must NOT be merged by a "this way" consumer.
//!
//! Oracle (constructed to collide on the graveyard):
//!   "Mill three cards, then sacrifice all creatures you control, then you gain
//!    life equal to the number of creatures sacrificed this way."
//!
//! CR 701.17a (mill) and CR 701.21a (sacrifice) BOTH put their objects into the
//! graveyard. The milled cards are creatures, so a pure type filter cannot
//! separate them either — the ONLY discriminator is the producer ACTION. Under
//! the old zone-only model both the milled creatures and the sacrificed
//! creatures land in `Zone::Graveyard`, so a `landed_in: Some(Graveyard)`
//! "sacrificed this way" consumer would count BOTH and over-gain life. Binding
//! the consumer to the `Sacrificed` cause (stamped from the `Effect::Sacrifice`
//! producer, CR 614.6) counts only the sacrificed creatures.
//!
//! Regression assertion: 2 creature cards milled + 1 battlefield creature
//! sacrificed → life gain MUST equal 1 (the sacrificed creature), NOT 3 (the
//! merged graveyard set the zone model would see).

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::AbilityKind;
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;

const ORACLE: &str = "Mill three cards, then sacrifice all creatures you control, then you gain life equal to the number of creatures sacrificed this way.";

/// CR 608.2c + CR 614.6: milled creatures and sacrificed creatures both reach
/// the graveyard, but the "sacrificed this way" life-gain binds to the
/// `Sacrificed` cause and counts only the single sacrificed creature.
#[test]
fn living_death_sacrificed_this_way_excludes_same_graveyard_milled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two CREATURE cards on top of the library → MILLED this way (land in the
    // graveyard, producer action `Milled`). Marked Creature so the type filter
    // ("creatures sacrificed this way") cannot exclude them — only the cause can.
    let milled_a = scenario.add_card_to_library_top(P0, "Milled Bear A");
    let milled_b = scenario.add_card_to_library_top(P0, "Milled Bear B");
    // A third milled card (non-creature padding) so "mill three" has a full set.
    scenario.add_card_to_library_top(P0, "Milled Padding");

    // One battlefield creature → SACRIFICED this way (lands in the graveyard,
    // producer action `Sacrificed`).
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);

    // Non-creature source so the spell source never matches the creature filter.
    let source = scenario.add_basic_land(P0, ManaColor::Black);

    let mut runner = scenario.build();
    // Mark the two milled cards as creatures so the consumer's Creature filter
    // would match them if the zone model leaked them into the count.
    for id in [milled_a, milled_b] {
        mark_creature(runner.state_mut(), id);
    }

    let starting_life = runner.state().players[0].life;

    let def = parse_effect_chain(ORACLE, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("mill+sacrifice lifegain variant must resolve");

    assert_eq!(
        runner.state().players[0].life,
        starting_life + 1,
        "life gain must equal the single creature SACRIFICED this way, not the two \
         creatures MILLED into the same graveyard (cause discriminates, zone does not)"
    );
}

fn mark_creature(state: &mut engine::types::game_state::GameState, id: ObjectId) {
    let obj = state.objects.get_mut(&id).expect("milled card exists");
    if !obj.card_types.core_types.contains(&CoreType::Creature) {
        obj.card_types.core_types.push(CoreType::Creature);
    }
    obj.power = Some(2);
    obj.toughness = Some(2);
}

//! Fuse integration coverage against real split-card fixture data.
//!
//! `Breaking // Entering` is useful here because it proves CR 702.102d order:
//! the left half mills first, then the right half can reanimate a creature card
//! that only became a legal graveyard target because the left half resolved.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastingVariant, StackEntryKind};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn pool_units(mana: &[ManaType]) -> Vec<ManaUnit> {
    let dummy = ObjectId(0);
    mana.iter()
        .map(|m| ManaUnit::new(*m, dummy, false, vec![]))
        .collect()
}

#[test]
fn fused_breaking_entering_combines_cost_characteristics_and_resolves_left_then_right() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let breaking = scenario.add_real_card(P0, "Breaking", Zone::Hand, db);
    let milled_creature = scenario.add_real_card(P1, "Grizzly Bears", Zone::Library, db);
    for _ in 0..7 {
        scenario.add_real_card(P1, "Lightning Bolt", Zone::Library, db);
    }
    scenario.with_mana_pool(
        P0,
        pool_units(&[
            ManaType::Blue,
            ManaType::Black,
            ManaType::Black,
            ManaType::Red,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]),
    );
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let commit = runner
        .cast(breaking)
        .casting_variant(CastingVariant::Fuse)
        .target_player(P1)
        .target_object(milled_creature)
        .commit();

    let selected = commit
        .selected_casting_variant()
        .expect("fuse should be selected through CastingVariantChoice");
    assert_eq!(selected.variant, CastingVariant::Fuse);
    assert_eq!(
        selected.mana_cost.mana_value(),
        8,
        "CR 702.102c: fused choice cost includes both halves"
    );

    let state = commit.state();
    assert_eq!(
        state.players[0].mana_pool.total(),
        0,
        "the exact fused cost should be paid before the spell reaches priority"
    );
    assert_eq!(state.stack.len(), 1, "fused spell should be on the stack");
    let stack_entry = state.stack.last().unwrap();
    let StackEntryKind::Spell {
        casting_variant,
        actual_mana_spent,
        ..
    } = &stack_entry.kind
    else {
        panic!("expected fused split card to be a spell stack entry");
    };
    assert_eq!(*casting_variant, CastingVariant::Fuse);
    assert_eq!(
        *actual_mana_spent, 8,
        "CR 702.102c: fused total cost includes both halves"
    );

    let stack_object = &state.objects[&stack_entry.source_id];
    assert!(stack_object
        .card_types
        .core_types
        .contains(&CoreType::Sorcery));
    assert!(stack_object.color.contains(&ManaColor::Blue));
    assert!(stack_object.color.contains(&ManaColor::Black));
    assert!(stack_object.color.contains(&ManaColor::Red));
    assert_eq!(
        stack_object.zone,
        Zone::Stack,
        "CR 702.102b + CR 709.4d: fused characteristics must be visible on stack"
    );
    assert_eq!(
        state.objects[&milled_creature].zone,
        Zone::Library,
        "the right-half target is only legal after Breaking mills it"
    );

    let outcome = commit.resolve();

    // CR 608.2c + CR 702.102d: Breaking mills first; Entering then reanimates
    // the creature that the left half put into the graveyard.
    outcome.assert_zone(&[milled_creature], Zone::Battlefield);
    assert_eq!(outcome.state().objects[&milled_creature].controller, P0);
    outcome.assert_zone(&[breaking], Zone::Graveyard);
}

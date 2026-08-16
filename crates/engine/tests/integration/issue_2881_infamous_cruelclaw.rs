//! Issue #2881 — The Infamous Cruelclaw's combat-damage trigger must exile from
//! the top of the controller's library until a nonland card is exiled, then
//! offer to cast that card by discarding a card instead of paying its mana cost.
//!
//! Oracle:
//!   Menace
//!   Whenever The Infamous Cruelclaw deals combat damage to a player, exile cards
//!   from the top of your library until you exile a nonland card. You may cast that
//!   card by discarding a card rather than paying its mana cost.
//!
//! Root cause: `ExileFromTopUntil` resolved `TargetFilter::Controller` against
//! `scoped_player` (the damaged player on combat-damage triggers) instead of
//! the ability controller's library per CR 109.5.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{AbilityCost, Effect, QuantityExpr};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::run_combat;

fn put_on_library_top(
    state: &mut engine::types::game_state::GameState,
    obj_id: ObjectId,
    owner: PlayerId,
) {
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(state, obj_id, Zone::Library, &mut events);
    let player = state.players.iter_mut().find(|p| p.id == owner).unwrap();
    player.library.retain(|id| *id != obj_id);
    player.library.insert(0, obj_id);
}

const CRUELCLAW_ORACLE: &str = "Menace\n\
Whenever The Infamous Cruelclaw deals combat damage to a player, exile cards \
from the top of your library until you exile a nonland card. You may cast that \
card by discarding a card rather than paying its mana cost.";

/// CR 510.2 + CR 603.2 + CR 701.13a: unblocked combat damage exiles from the
/// controller's library until a nonland is hit, even though the trigger stamps
/// the damaged player as `scoped_player`.
#[test]
fn infamous_cruelclaw_combat_damage_exiles_until_nonland() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let cruelclaw = scenario
        .add_creature_from_oracle(P0, "The Infamous Cruelclaw", 3, 3, CRUELCLAW_ORACLE)
        .id();

    // Library top → bottom: Island, Island, Lightning Bolt (nonland stop).
    let island1 = scenario.add_basic_land(P0, ManaColor::Blue);
    let island2 = scenario.add_basic_land(P0, ManaColor::Blue);
    let bolt = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();

    let mut runner = scenario.build();
    put_on_library_top(runner.state_mut(), bolt, P0);
    put_on_library_top(runner.state_mut(), island2, P0);
    put_on_library_top(runner.state_mut(), island1, P0);

    assert!(
        !runner.state().objects[&cruelclaw]
            .trigger_definitions
            .is_empty(),
        "Cruelclaw must install combat-damage trigger"
    );
    let execute = runner.state().objects[&cruelclaw].trigger_definitions[0]
        .definition
        .execute
        .as_ref()
        .expect("trigger execute");
    let cast = execute
        .sub_ability
        .as_ref()
        .expect("optional cast sub-ability");
    let Effect::CastFromZone {
        alt_ability_cost, ..
    } = cast.effect.as_ref()
    else {
        panic!("expected CastFromZone sub-ability, got {:?}", cast.effect);
    };
    assert!(
        matches!(
            alt_ability_cost,
            Some(AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                ..
            })
        ),
        "expected discard alt cost on cast offer, got {alt_ability_cost:?}"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        3,
        "precondition: three cards on library top"
    );

    run_combat(&mut runner, vec![cruelclaw], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&island1].zone,
        Zone::Exile,
        "first exiled card (land) must leave library"
    );
    assert_eq!(
        runner.state().objects[&island2].zone,
        Zone::Exile,
        "second exiled card (land) must leave library"
    );
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Exile,
        "nonland hit must be exiled"
    );
}

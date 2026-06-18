//! Regression for issue #3316: Stinging Study must correctly resolve X
//! as the mana value of a commander you own on the battlefield or in the command zone.
//!
//! https://github.com/phase-rs/phase/issues/3316
//!
//! This test verifies that the QuantityRef::CommanderManaValue resolver correctly
//! enumerates commanders in the command zone after zone_object_ids(Zone::Command)
//! was fixed to return state.command_zone. The test directly calls the resolver
//! with a commander in the command zone and asserts the correct mana value is returned.

use engine::game::quantity::resolve_quantity;
use engine::game::zones::create_object;
use engine::types::ability::{ControllerRef, QuantityExpr, QuantityRef};
use engine::types::card_type::{CoreType, Supertype};
use engine::types::format::FormatConfig;
use engine::types::identifiers::CardId;
use engine::types::mana::ManaCost;
use engine::types::zones::Zone;
use engine::types::GameState;
use engine::types::PlayerId;

#[test]
fn stinging_study_command_zone_commander_resolves_correct_mana_value() {
    // Create a commander format game state
    let mut state = GameState::new(FormatConfig::commander(), 4, 42);

    // Create a 5-mana commander in the command zone
    let commander_id = create_object(
        &mut state,
        CardId(9001),
        PlayerId(0),
        "Test Commander".to_string(),
        Zone::Command,
    );
    {
        let obj = state.objects.get_mut(&commander_id).unwrap();
        obj.card_types.supertypes.push(Supertype::Legendary);
        obj.card_types.core_types.push(CoreType::Creature);
        obj.is_commander = true;
        obj.mana_cost = ManaCost::generic(5);
    }

    // Create the QuantityRef::CommanderManaValue expression
    let expr = QuantityExpr::Ref {
        qty: QuantityRef::CommanderManaValue {
            owner: ControllerRef::You,
        },
    };

    // Resolve the quantity - should return 5 (commander's mana value)
    let x = resolve_quantity(&state, &expr, PlayerId(0), commander_id);

    assert_eq!(x, 5, "X should resolve to commander's mana value (5)");
}

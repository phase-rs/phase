//! Regression for GitHub issue #4251 — Dungeon of the Mad Mage rooms Deep Mines
//! and Mad Wizard's Lair did not resolve properly.
//!
//! Mad Wizard's Lair (room 8): "Draw three cards and reveal them. You may cast
//! one of them without paying its mana cost." Was `Effect::Unimplemented`, so
//! venturing into the final room did nothing.

use engine::game::dungeon::{dungeon_sentinel_id, room_effects, DungeonId};
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply_as_current;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::zones::Zone;

fn assert_no_unimplemented(ability: &ResolvedAbility) {
    assert!(
        !matches!(ability.effect, Effect::Unimplemented { .. }),
        "unexpected Unimplemented: {:?}",
        ability.effect
    );
    if let Some(sub) = ability.sub_ability.as_ref() {
        assert_no_unimplemented(sub);
    }
}

#[test]
fn deep_mines_room_effect_is_implemented_scry_three() {
    let (ability, _) = room_effects(
        DungeonId::DungeonOfTheMadMage,
        7,
        dungeon_sentinel_id(P0),
        P0,
    );
    assert_no_unimplemented(&ability);
    assert!(
        matches!(
            ability.effect,
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 3 },
                ..
            }
        ),
        "Deep Mines must scry 3, got {:?}",
        ability.effect
    );
}

#[test]
fn mad_wizards_lair_draws_reveals_and_casts_one_revealed_card_free() {
    let mut scenario = GameScenario::new();
    for i in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {i}"));
    }
    // A castable one-drop in the library top after three draws.
    let bolt = scenario.add_card_to_library_top(P0, "Shock");
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&bolt).unwrap();
        obj.card_types.core_types = vec![CoreType::Instant];
        obj.base_card_types = obj.card_types.clone();
        obj.mana_cost = engine::types::mana::ManaCost::Cost {
            shards: vec![engine::types::mana::ManaCostShard::Red],
            generic: 0,
        };
    }

    let hand_before = runner.state().players[0].hand.len();
    let (ability, _) = room_effects(
        DungeonId::DungeonOfTheMadMage,
        8,
        dungeon_sentinel_id(P0),
        P0,
    );

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Mad Wizard's Lair begins resolving");

    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 3,
        "room must draw three cards before the optional cast"
    );
    assert_eq!(
        runner.state().last_revealed_ids.len(),
        3,
        "the three drawn cards must be recorded as revealed (events={events:?})"
    );

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "optional free cast must pause for accept/decline, got {:?}",
        runner.state().waiting_for
    );

    apply_as_current(
        runner.state_mut(),
        GameAction::DecideOptionalEffect { accept: true },
    )
    .expect("accept optional free cast");

    let WaitingFor::EffectZoneChoice {
        cards,
        count,
        up_to,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "accepting must offer a hand pick among the revealed cards, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(count, 1);
    assert!(up_to);
    assert_eq!(
        cards.len(),
        3,
        "all three revealed hand cards must be eligible"
    );
    assert!(cards.contains(&bolt), "Shock must be in the cast pool");

    apply_as_current(
        runner.state_mut(),
        GameAction::SelectCards { cards: vec![bolt] },
    )
    .expect("select Shock to cast free");

    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Stack,
        "the chosen revealed card must be cast during resolution"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 2,
        "the two unchosen revealed cards must remain in hand"
    );
}

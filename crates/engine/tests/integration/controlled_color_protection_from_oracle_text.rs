//! Empty-Shrine Kannushi and Pledge of Loyalty ("protection from each color among permanents
//! you control") end to end from real Oracle text: the exported cards, cast through the
//! public path, must protect their recipient from a color a permanent they control has.
//!
//! This module deliberately names no type introduced by the fix, so it still compiles when
//! the production change is removed and fails there at runtime (the cards fall back to
//! unsupported and the recipient stays targetable). The detailed live-set scenarios live in
//! `empty_shrine_kannushi_pledge`.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::targeting::find_legal_targets;
use engine::types::ability::{TargetFilter, TargetRef, TypedFilter};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn fund(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
    for mana in [ManaType::White, ManaType::Red, ManaType::Colorless] {
        for _ in 0..10 {
            state
                .add_mana_to_pool(player, ManaUnit::new(mana, ObjectId(0), false, vec![]))
                .unwrap();
        }
    }
}

// CR 702.16b: a permanent with protection from a color can't be the target of spells of that
// color. CR 105.2 + CR 611.3a: "each color among permanents you control" reads the colors the
// controller's permanents have (Fervor, a red enchantment, here).
#[test]
fn controlled_color_protection_stops_a_red_spell_from_targeting_the_recipient() {
    for pledge in [false, true] {
        let db = shared_card_db().expect("integration card fixture must load");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_real_card(P0, "Fervor", Zone::Battlefield, db);
        let host = scenario
            .add_creature(P0, "Colorless host fixture", 4, 6)
            .id();
        let sibling = scenario.add_creature(P0, "Unprotected fixture", 4, 6).id();
        let grant = scenario.add_real_card(
            P0,
            if pledge {
                "Pledge of Loyalty"
            } else {
                "Empty-Shrine Kannushi"
            },
            Zone::Hand,
            db,
        );
        let bolt = scenario.add_real_card(P1, "Lightning Bolt", Zone::Hand, db);
        let mut runner = scenario.build();
        fund(&mut runner, P0);
        let cast = runner.cast(grant);
        let protected = if pledge {
            cast.target_object(host).resolve();
            host
        } else {
            cast.resolve();
            grant
        };
        assert_eq!(
            runner.state().objects[&protected].zone,
            Zone::Battlefield,
            "the granting card must have resolved onto the battlefield"
        );
        let legal = find_legal_targets(
            runner.state(),
            &TargetFilter::Typed(TypedFilter::creature()),
            P1,
            bolt,
        );
        assert!(
            legal.contains(&TargetRef::Object(sibling)),
            "reach guard: an unprotected creature is still a legal target"
        );
        assert!(
            !legal.contains(&TargetRef::Object(protected)),
            "pledge={pledge}: the recipient must have protection from red via the controlled Fervor"
        );
    }
}

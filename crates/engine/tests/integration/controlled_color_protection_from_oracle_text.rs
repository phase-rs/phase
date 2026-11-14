//! Empty-Shrine Kannushi and Pledge of Loyalty ("protection from each color among permanents
//! you control") end to end from real Oracle text: the exported cards, cast through the
//! public path, must protect their recipient from a color a permanent they control has.
//!
//! This module deliberately names no type introduced by the fix, so it still compiles when
//! the production change is removed and fails there at runtime (the cards fall back to
//! unsupported and the recipient stays targetable). The detailed live-set scenarios live in
//! `empty_shrine_kannushi_pledge`.

use engine::database::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::targeting::find_legal_targets;
use engine::types::ability::{TargetFilter, TargetRef, TypedFilter};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
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

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn db() -> &'static CardDatabase {
    shared_card_db().expect("integration card fixture must load")
}

fn colors(runner: &GameRunner, host: ObjectId) -> Vec<ManaColor> {
    let mut result: Vec<_> = runner.state().objects[&host]
        .keywords
        .iter()
        .filter_map(|keyword| match keyword {
            Keyword::Protection(ProtectionTarget::Color(color)) => Some(*color),
            _ => None,
        })
        .collect();
    result.sort();
    result
}

fn prepare(runner: &mut GameRunner, player: PlayerId) {
    assert!(runner.state().stack.is_empty());
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
    for mana in [ManaType::White, ManaType::Colorless] {
        for _ in 0..10 {
            state
                .add_mana_to_pool(player, ManaUnit::new(mana, ObjectId(0), false, vec![]))
                .unwrap();
        }
    }
}

fn complete(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty(), "stack must settle");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
}

fn cast(runner: &mut GameRunner, player: PlayerId, spell: ObjectId) {
    prepare(runner, player);
    let outcome = runner.cast(spell).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    complete(runner);
}

fn cast_on(runner: &mut GameRunner, player: PlayerId, spell: ObjectId, target: ObjectId) {
    prepare(runner, player);
    let outcome = runner.cast(spell).target_object(target).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    complete(runner);
}

fn attached(runner: &GameRunner, aura: ObjectId, host: ObjectId) {
    assert_eq!(runner.state().objects[&aura].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&aura].attached_to, Some(host.into()));
}
// CR 613.7 + CR 613.1f + CR 702.16c: Humility is applied after Pledge of
// Loyalty, so the layer pass removes the protection Pledge granted. The Aura
// itself is not a creature and keeps its static, but that grant no longer
// survives on the host — attachment legality must not reconstruct it.
#[test]
fn kannushi_pledge_humility_then_pacifism_reads_surviving_protection() {
    let mut s = scenario();
    let host = s.add_creature(P1, "Colorless host fixture", 4, 6).id();
    let pledge = s.add_real_card(P0, "Pledge of Loyalty", Zone::Hand, db());
    let humility = s.add_real_card(P0, "Humility", Zone::Hand, db());
    let pacifism = s.add_real_card(P1, "Pacifism", Zone::Hand, db());
    let mut runner = s.build();
    cast_on(&mut runner, P0, pledge, host);
    assert_eq!(colors(&runner, host), vec![ManaColor::White]);
    attached(&runner, pledge, host);
    // Positive reach guard: while the grant survives, the host is not a legal
    // Pacifism target.
    assert!(!find_legal_targets(
        runner.state(),
        &TargetFilter::Typed(TypedFilter::creature()),
        P1,
        pacifism,
    )
    .contains(&TargetRef::Object(host)));
    cast(&mut runner, P0, humility);
    assert!(runner.state().objects[&host]
        .keywords
        .iter()
        .all(|keyword| !matches!(keyword, Keyword::Protection(_))));
    attached(&runner, pledge, host);
    // Humility removed the protection, so Pacifism may target the host.
    assert!(find_legal_targets(
        runner.state(),
        &TargetFilter::Typed(TypedFilter::creature()),
        P1,
        pacifism,
    )
    .contains(&TargetRef::Object(host)));
    cast_on(&mut runner, P1, pacifism, host);
    attached(&runner, pacifism, host);
}

// CR 613.1f + CR 702.16c: printed protection removed by Humility is gone, so
// an Aura may attach. The old printed-keyword scan resurrected it.
#[test]
fn humility_removed_printed_protection_does_not_block_pacifism() {
    let mut s = scenario();
    let knight = s.add_real_card(P1, "Black Knight", Zone::Battlefield, db());
    let humility = s.add_real_card(P0, "Humility", Zone::Hand, db());
    let pacifism = s.add_real_card(P0, "Pacifism", Zone::Hand, db());
    let mut runner = s.build();
    assert_eq!(colors(&runner, knight), vec![ManaColor::White]);
    assert!(!find_legal_targets(
        runner.state(),
        &TargetFilter::Typed(TypedFilter::creature()),
        P0,
        pacifism,
    )
    .contains(&TargetRef::Object(knight)));
    cast(&mut runner, P0, humility);
    assert!(runner.state().objects[&knight]
        .keywords
        .iter()
        .all(|keyword| !matches!(keyword, Keyword::Protection(_))));
    assert!(find_legal_targets(
        runner.state(),
        &TargetFilter::Typed(TypedFilter::creature()),
        P0,
        pacifism,
    )
    .contains(&TargetRef::Object(knight)));
    cast_on(&mut runner, P0, pacifism, knight);
    attached(&runner, pacifism, knight);
}

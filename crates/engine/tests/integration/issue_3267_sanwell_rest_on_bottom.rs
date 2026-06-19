//! Regression test for GitHub issue #3267 — Sanwell, Avenger Ace.
//!
//! "Whenever Sanwell becomes tapped, exile the top six cards of your library.
//! You may cast a Vehicle or artifact creature spell from among them. Then put
//! the rest on the bottom of your library in a random order."
//!
//! CR 608.2c + CR 401.4: Uncast cards from the exile step must return to the
//! library bottom via `PutAtLibraryPosition { ExiledBySource }`, not linger in
//! exile linked to the source.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::CardId;
use engine::types::zones::Zone;

const SANWELL_TRIGGER_BODY: &str = "exile the top six cards of your library. You may cast a Vehicle or artifact creature spell from among them. Then put the rest on the bottom of your library in a random order.";

fn sanwell_execute() -> engine::types::ability::AbilityDefinition {
    parse_effect_chain(SANWELL_TRIGGER_BODY, AbilityKind::Spell)
}

fn add_library_card(
    state: &mut engine::types::game_state::GameState,
    name: &str,
    artifact_creature: bool,
) -> engine::types::identifiers::ObjectId {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let card_id = CardId(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let id = create_object(state, card_id, P0, name.to_string(), Zone::Library);
    if artifact_creature {
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact, CoreType::Creature];
    }
    id
}

#[test]
fn sanwell_declined_cast_puts_exiled_cards_on_library_bottom() {
    let execute = sanwell_execute();
    let cast = execute.sub_ability.as_ref().expect("cast branch");
    let cleanup = cast.sub_ability.as_ref().expect("cleanup sub");
    let Effect::PutAtLibraryPosition { target, count, .. } = &*cleanup.effect else {
        panic!(
            "expected PutAtLibraryPosition cleanup, got {:?}",
            cleanup.effect
        );
    };
    assert_eq!(*target, TargetFilter::ExiledBySource);
    assert_eq!(*count, QuantityExpr::Fixed { value: 0 });

    let scenario = GameScenario::new();
    let mut runner = scenario.build();
    let source = {
        let state = runner.state_mut();
        create_object(
            state,
            CardId(99),
            P0,
            "Sanwell, Avenger Ace".to_string(),
            Zone::Battlefield,
        )
    };

    let bottom_marker = add_library_card(runner.state_mut(), "Bottom Marker", false);

    let mut exiled_top = Vec::new();
    for idx in 0..6 {
        let artifact_creature = idx == 2;
        let id = add_library_card(
            runner.state_mut(),
            &format!("Library Card {idx}"),
            artifact_creature,
        );
        exiled_top.push(id);
    }

    let resolved = build_resolved_from_def(&execute, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("Sanwell trigger resolution");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "optional cast prompt expected before cleanup"
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline optional cast");

    let state = runner.state();
    for id in &exiled_top {
        assert_eq!(
            state.objects[id].zone,
            Zone::Library,
            "{id:?} should be on library bottom after decline, not exile"
        );
        assert!(
            state.players[P0.0 as usize].library.contains(id),
            "{id:?} should remain in controller library"
        );
    }
    assert_eq!(
        state.objects[&bottom_marker].zone,
        Zone::Library,
        "untouched library card should stay in library"
    );
    assert!(
        !state
            .exile_links
            .iter()
            .any(|link| link.source_id == source),
        "cleanup should clear source-linked exile entries"
    );
}

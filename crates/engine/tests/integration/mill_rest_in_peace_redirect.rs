//! Phase C1 (zone-change pipeline) discriminating test for the
//! mill-honors-Moved-redirect bug-fix.
//!
//! Before Phase C1, `mill::apply_mill_after_replacement` delivered each milled
//! card with a bare `zones::move_to_zone(state, obj_id, destination, events)`.
//! That raw move never proposed a per-card `ZoneChange`, so `Moved`-level
//! redirects ("if a card would be put into a graveyard from anywhere, exile it
//! instead" — Rest in Peace / Leyline of the Void class) were silently dropped
//! for milled cards: a milled card landed in the graveyard even with Rest in
//! Peace on the battlefield. (PLAN §8 Risk #1; confirmed bug.)
//!
//! Phase C1 routes each milled card through `zone_pipeline::move_object`, which
//! proposes the inner `ZoneChange` and consults the `Moved` replacements before
//! delivery.
//!
//! This drives the real mill pipeline (resolve `Effect::Mill` -> Mill-level
//! replacement pass -> per-card `move_object` -> `replace_event` graveyard->exile
//! redirect -> deliver) with a global Rest-in-Peace-style replacement in play,
//! and asserts the milled cards end in EXILE, not the graveyard. It FAILS on the
//! old raw-`move_to_zone` delivery (cards reach the graveyard) and passes through
//! the pipeline.

use engine::game::effects::mill;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, ReplacementDefinition, ResolvedAbility,
    TargetFilter, TargetRef,
};
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

/// CR 614.6: "If a card would be put into a graveyard from anywhere, exile it
/// instead." (Rest in Peace / Leyline of the Void class.) Modeled exactly as the
/// parser builds it: a `Moved` replacement scoped to `destination_zone(Graveyard)`
/// whose execute is a self `ChangeZone` to Exile. `valid_card` left unset = global.
fn graveyard_exile_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Exile,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
            },
        ))
        .description(
            "If a card would be put into a graveyard from anywhere, exile it instead.".to_string(),
        )
}

#[test]
fn mill_honors_rest_in_peace_graveyard_to_exile_redirect() {
    let mut scenario = GameScenario::new();

    // Rest in Peace on the battlefield supplies the global graveyard->exile
    // Moved replacement.
    scenario
        .add_creature(P0, "Rest in Peace", 0, 0)
        .as_enchantment()
        .with_replacement_definition(graveyard_exile_replacement());

    // Give the mill victim (P1) a library to mill.
    let milled: Vec<_> = (0..3)
        .map(|i| scenario.add_card_to_library_top(P1, &format!("Milled Card {i}")))
        .collect();

    let mut runner = scenario.build();

    // Mill 3 from P1's library through the real Mill effect pipeline.
    let ability = ResolvedAbility::new(
        Effect::Mill {
            count: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            destination: Zone::Graveyard,
        },
        vec![TargetRef::Player(P1)],
        // Mill source: an arbitrary object id; mill attribution anchors on the
        // milled card itself, so the source object need not exist.
        runner.state().objects.keys().next().copied().unwrap(),
        P0,
    );

    let mut events = Vec::new();
    mill::resolve(runner.state_mut(), &ability, &mut events).unwrap();

    let state = runner.state();
    // CR 614.6: every milled card must have been redirected to exile by the
    // Moved replacement — the discriminating assertion. The old raw delivery put
    // them in the graveyard (the redirect never fired).
    for &id in &milled {
        let obj = state.objects.get(&id).expect("milled card still exists");
        assert_eq!(
            obj.zone,
            Zone::Exile,
            "a milled card must honor the Rest in Peace graveyard->exile Moved redirect"
        );
    }
    assert!(
        state.players[1].graveyard.is_empty(),
        "no milled card reached the graveyard — all were exiled by the redirect"
    );
    assert_eq!(
        state.players[1].library.len(),
        0,
        "all three cards left the library"
    );
}

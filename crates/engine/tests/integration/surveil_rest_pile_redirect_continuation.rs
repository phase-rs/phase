//! Phase C6 (zone-change pipeline) discriminating tests for the rest-pile
//! batch migration: the surveil graveyard "rest pile" now routes through
//! `zone_pipeline::move_objects_simultaneously_then`, so each unkept card's own
//! `Moved` redirects fire (Rest in Peace / Leyline of the Void: "would be put
//! into a graveyard from anywhere → exile instead"), and the post-loop
//! kept-on-top library reorder runs exactly once on batch completion — including
//! when a per-card CR 616.1 ordering choice pauses the pile mid-delivery.
//!
//! Before C6 the handler delivered every unkept card with a bare
//! `zones::move_to_zone(.., Zone::Graveyard, ..)`, which proposed no per-card
//! ZoneChange and silently skipped the redirects; the kept-on-top reorder ran
//! inline at the end of the loop, so a mid-pile pause (two simultaneous
//! redirects) could not be supported at all (that is why C6 was deferred twice).
//!
//! Two tests:
//!  1. `surveil_rest_pile_honors_graveyard_exile_redirect` — single redirect: the
//!     unkept cards land in EXILE, the kept card stays on top of the library, and
//!     no card reaches the graveyard. The synchronous (never-paused) path.
//!  2. `surveil_rest_pile_under_two_redirects_runs_keep_on_top_cleanup_once` — two
//!     simultaneous redirects make every unkept card prompt a CR 616.1 ordering
//!     choice; answering each delivers ALL of them to exile (none stranded), the
//!     parked batch tail fully drains, and the kept-on-top reorder runs exactly
//!     once (the kept card is on top, present exactly once, not duplicated).

use engine::game::effects::surveil;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, ReplacementDefinition, ResolvedAbility,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

/// CR 614.6: "If a card would be put into a graveyard from anywhere, exile it
/// instead." (Rest in Peace / Leyline of the Void class.)
fn graveyard_exile_replacement(description: &str) -> ReplacementDefinition {
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
        .description(description.to_string())
}

fn surveil_ability(count: i32, source: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::Surveil {
            count: QuantityExpr::Fixed { value: count },
            target: TargetFilter::Controller,
        },
        vec![],
        source,
        P1,
    )
}

/// Lay out P1's library top-to-bottom as `[keep, rest0, rest1, below]` and return
/// those four ids. `add_card_to_library_top` inserts at index 0, so add bottom
/// first. Surveil 3 looks at the top three (`keep`, `rest0`, `rest1`).
fn library_keep_rest_below(
    scenario: &mut GameScenario,
) -> (ObjectId, ObjectId, ObjectId, ObjectId) {
    let below = scenario.add_card_to_library_top(P1, "Below Window");
    let rest1 = scenario.add_card_to_library_top(P1, "Rest 1");
    let rest0 = scenario.add_card_to_library_top(P1, "Rest 0");
    let keep = scenario.add_card_to_library_top(P1, "Keep On Top");
    (keep, rest0, rest1, below)
}

#[test]
fn surveil_rest_pile_honors_graveyard_exile_redirect() {
    let mut scenario = GameScenario::new();

    // A single global graveyard→exile redirect controlled by the opponent.
    scenario
        .add_creature(P0, "Rest in Peace", 0, 0)
        .as_enchantment()
        .with_replacement_definition(graveyard_exile_replacement(
            "If a card would be put into a graveyard from anywhere, exile it instead.",
        ));

    let (keep, rest0, rest1, below) = library_keep_rest_below(&mut scenario);

    let mut runner = scenario.build();
    let source = runner.state().objects.keys().next().copied().unwrap();

    // Surveil 3 looks at [keep, rest0, rest1]; keep `keep` on top, the rest go to
    // the graveyard (→ exile under the redirect).
    let ability = surveil_ability(3, source);
    let mut events = Vec::new();
    surveil::resolve(runner.state_mut(), &ability, &mut events).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::SurveilChoice { .. }
    ));

    runner
        .act(GameAction::SelectCards { cards: vec![keep] })
        .expect("submit the surveil keep-on-top selection");

    let state = runner.state();
    // The unkept cards honored the redirect to exile, not the graveyard.
    for &id in &[rest0, rest1] {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Exile,
            "an unkept surveil card must honor the graveyard->exile Moved redirect"
        );
    }
    assert!(
        state.players[1].graveyard.is_empty(),
        "no surveil card may reach the graveyard under the redirect"
    );
    // The kept card rests on top; the below-window card is untouched beneath.
    let library: Vec<ObjectId> = state.players[1].library.iter().copied().collect();
    assert_eq!(
        library,
        vec![keep, below],
        "kept card stays on top exactly once, below-window card untouched"
    );
}

#[test]
fn surveil_rest_pile_under_two_redirects_runs_keep_on_top_cleanup_once() {
    let mut scenario = GameScenario::new();

    // Two simultaneously-applicable graveyard→exile redirects: CR 616.1 forces an
    // ordering prompt on EVERY unkept card, pausing the rest-pile batch mid-pile.
    scenario
        .add_creature(P0, "Rest in Peace", 0, 0)
        .as_enchantment()
        .with_replacement_definition(graveyard_exile_replacement(
            "If a card would be put into a graveyard from anywhere, exile it instead. (RIP)",
        ));
    scenario
        .add_creature(P0, "Leyline of the Void", 0, 0)
        .as_enchantment()
        .with_replacement_definition(graveyard_exile_replacement(
            "If a card would be put into a graveyard from anywhere, exile it instead. (Leyline)",
        ));

    let (keep, rest0, rest1, below) = library_keep_rest_below(&mut scenario);

    let mut runner = scenario.build();
    let source = runner.state().objects.keys().next().copied().unwrap();

    // Surveil 3 looks at [keep, rest0, rest1]; keep `keep` on top, rest0 + rest1
    // form the graveyard rest pile (each prompts a CR 616.1 ordering choice).
    let ability = surveil_ability(3, source);
    let mut events = Vec::new();
    surveil::resolve(runner.state_mut(), &ability, &mut events).unwrap();
    runner
        .act(GameAction::SelectCards { cards: vec![keep] })
        .expect("submit the surveil keep-on-top selection");

    // CR 616.1: each unkept card surfaces an ordering prompt between the two
    // applicable redirects. Answer each (index 0); bounded to fail loudly on a
    // regression that loops or re-prompts indefinitely instead of hanging.
    let mut prompts_answered = 0;
    for _ in 0..10 {
        if !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("answer the CR 616.1 ordering prompt");
        prompts_answered += 1;
    }

    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "all replacement prompts must be answerable; still waiting after {prompts_answered}"
    );
    assert!(
        prompts_answered >= 1,
        "two simultaneously-applicable redirects must surface a CR 616.1 ordering prompt"
    );
    // No unkept card may strand; both honored the redirect to exile.
    for &id in &[rest0, rest1] {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Exile,
            "every unkept surveil card must honor the redirect — none may strand on a pause"
        );
    }
    assert!(
        state.players[1].graveyard.is_empty(),
        "no unkept card may reach the graveyard under the redirects"
    );
    assert!(
        state.pending_batch_deliveries.is_none(),
        "the parked rest-pile tail must be fully drained"
    );
    // The discriminating C6 assertion: the post-loop kept-on-top reorder ran
    // exactly ONCE across the pause boundary. The kept card is on top, present
    // exactly once (not duplicated by a double-run, not missing by a never-run).
    let library: Vec<ObjectId> = state.players[1].library.iter().copied().collect();
    assert_eq!(
        library,
        vec![keep, below],
        "kept-on-top cleanup must run exactly once after the paused pile fully drains"
    );
}

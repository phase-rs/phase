//! Phase C1 follow-up (review finding): a mill batch must NOT strand cards when
//! a per-card `Moved` replacement surfaces a CR 616.1 ordering choice.
//!
//! With TWO simultaneously-applicable graveyard→exile redirects on the
//! battlefield (Rest in Peace + Leyline of the Void — a real, common
//! combination), the CR 616.1 materiality classifier
//! (`replacement.rs` — any destination-redirecting `Effect::ChangeZone` is
//! Unconditional-material) makes the engine prompt for ordering on EVERY milled
//! card, regardless of the identical outcome. The first milled card therefore
//! pauses the per-card delivery loop on `ZoneMoveResult::NeedsChoice`.
//!
//! Pre-fix, mill bailed with `return Ok(())` on that pause: cards 2..N were
//! silently stranded in the library with an orphaned pause. The fix parks the
//! undelivered tail in the active `BatchDelivery` frame and the
//! replacement-choice resume path (`handle_replacement_choice`) drains it after
//! each choice, re-parking when the next card surfaces its own prompt.
//!
//! This test mills 3 cards under both redirects, answers each CR 616.1 ordering
//! prompt, and asserts ALL milled cards left the library and ended in exile.
//! It FAILS (strands cards 2..3 in the library) on the pre-fix code.

use engine::game::effects::mill;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, ReplacementDefinition, ResolvedAbility,
    TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::replacements::ReplacementEvent;
use engine::types::resolution::{ResolutionFrame, ResolutionStateWire};
use engine::types::zones::{EtbTapState, Zone};

/// CR 614.6: "If a card would be put into a graveyard from anywhere, exile it
/// instead." (Rest in Peace / Leyline of the Void class — both cards carry this
/// exact replacement.)
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
                enters_modified_if: None,
            },
        ))
        .description(description.to_string())
}

#[test]
fn mill_under_two_graveyard_redirects_delivers_every_card_through_ordering_choices() {
    let mut scenario = GameScenario::new();

    // Two independent sources of the same graveyard→exile Moved replacement.
    // CR 616.1: both are simultaneously applicable to each milled card's
    // ZoneChange, so the engine prompts for ordering per card.
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

    let milled: Vec<_> = (0..3)
        .map(|i| scenario.add_card_to_library_top(P1, &format!("Milled Card {i}")))
        .collect();

    let mut runner = scenario.build();

    let ability = ResolvedAbility::new(
        Effect::Mill {
            count: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            destination: Zone::Graveyard,
        },
        vec![TargetRef::Player(P1)],
        runner.state().objects.keys().next().copied().unwrap(),
        P0,
    );

    let mut events = Vec::new();
    mill::resolve(runner.state_mut(), &ability, &mut events).unwrap();

    assert!(matches!(
        runner.state().resolution_stack.last(),
        Some(ResolutionFrame::BatchDelivery(_))
    ));
    let saved = serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
        .expect("paused BatchDelivery prompt serializes as v2");
    assert_eq!(saved["resolution_state_version"], 2);
    assert!(saved.get("pending_batch_deliveries").is_none());
    assert!(saved.get("resolution_frames").is_some());
    let restored: ResolutionStateWire =
        serde_json::from_value(saved).expect("v2 BatchDelivery prompt restores");
    *runner.state_mut() = restored.into_game_state();

    // CR 616.1: each milled card surfaces an ordering prompt between the two
    // applicable redirects. Answer each (index 0); bounded so a regression that
    // loops or re-prompts indefinitely fails loudly instead of hanging.
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
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            assert!(matches!(
                runner.state().resolution_stack.last(),
                Some(ResolutionFrame::BatchDelivery(_))
            ));
        }
    }

    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "all replacement prompts must be answerable; still waiting after {prompts_answered}"
    );
    assert!(
        prompts_answered >= 1,
        "the two simultaneously-applicable redirects must surface a CR 616.1 ordering prompt"
    );
    // The discriminating assertions: NO milled card may strand in the library;
    // every one of them was redirected to exile.
    assert!(
        state.players[1].library.is_empty(),
        "all milled cards must leave the library — none may be stranded by a mid-batch pause \
         (pre-fix: cards after the first prompt stranded)"
    );
    for &id in &milled {
        let obj = state.objects.get(&id).expect("milled card still exists");
        assert_eq!(
            obj.zone,
            Zone::Exile,
            "every milled card must honor the graveyard->exile redirect"
        );
    }
    assert!(
        state.players[1].graveyard.is_empty(),
        "no milled card may reach the graveyard under the redirects"
    );
    assert!(
        state.active_batch_delivery().is_none(),
        "the parked mill tail must be fully drained"
    );
}

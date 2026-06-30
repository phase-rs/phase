//! Phase C4 (zone-change pipeline) discriminating tests for the
//! mass-bounce-honors-Moved-redirect bug-fix.
//!
//! Before Phase C4, `bounce::resolve_all` delivered each mass-bounced permanent
//! with a bare `zones::move_to_zone(state, obj_id, destination, events)` under a
//! per-site comment claiming "no replacement-pipeline detour is needed because
//! mass-bounce events are not destruction events (CR 614.6 doesn't apply here)".
//! That justification was wrong by citation: CR 614.6 governs replacement
//! semantics generally, and CR 614.1 replacements watch zone-change *events*,
//! not only destruction. The raw move never proposed a per-object `ZoneChange`,
//! so `Moved` redirects watching the bounce destination ("if a creature would be
//! returned to a hand, exile it instead" class) were silently dropped. (PLAN §8
//! Risk #4.)
//!
//! Phase C4 routes the mass bounce through the shared
//! `zone_pipeline::move_objects_simultaneously` batch entry, which proposes each
//! inner `ZoneChange` and consults the `Moved` replacements before delivery.
//!
//! Two tests:
//!  1. `mass_bounce_honors_to_hand_redirect` — the discriminating test: a single
//!     "returned to hand -> exile instead" redirect sends every bounced creature
//!     to EXILE, not the hand. FAILS on the old raw `move_to_zone`.
//!  2. `mass_bounce_under_two_redirects_delivers_every_permanent_through_choices`
//!     — the pause/resume test: two simultaneous redirects make every bounced
//!     creature prompt a CR 616.1 ordering choice; answering each must deliver
//!     ALL of them (none stranded) and fully drain the parked batch tail.

use engine::game::effects::bounce;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, ResolvedAbility, TargetFilter,
    TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

/// CR 614.6: "If a permanent would be returned to a hand, exile it instead."
/// Modeled as a `Moved` replacement scoped to `destination_zone(Hand)` whose
/// execute is a self `ChangeZone` to Exile. `valid_card` left unset = global.
fn to_hand_exile_redirect(description: &str) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Hand)
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

fn bounce_all_creatures() -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::BounceAll {
            target: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: None,
                properties: vec![],
            }),
            destination: None,
            count: None,
        },
        vec![],
        engine::types::identifiers::ObjectId(9999),
        P0,
    )
}

#[test]
fn mass_bounce_honors_to_hand_redirect() {
    let mut scenario = GameScenario::new();

    // A single global "returned to hand -> exile" redirect.
    scenario
        .add_creature(P0, "Redirect Enchantment", 0, 0)
        .as_enchantment()
        .with_replacement_definition(to_hand_exile_redirect(
            "If a permanent would be returned to a hand, exile it instead.",
        ));

    let bear = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    let elf = scenario.add_creature(P1, "Llanowar Elves", 1, 1).id();

    let mut runner = scenario.build();
    let ability = bounce_all_creatures();
    let mut events = Vec::new();
    bounce::resolve_all(runner.state_mut(), &ability, &mut events).unwrap();

    let state = runner.state();
    // Discriminating assertions: both bounced creatures honored the redirect to
    // exile and reached neither owner's hand. The old raw move put them in hand.
    for &id in &[bear, elf] {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Exile,
            "a mass-bounced creature must honor the to-hand -> exile Moved redirect"
        );
    }
    assert!(
        state.players[0].hand.is_empty() && state.players[1].hand.is_empty(),
        "no bounced creature may reach a hand under the redirect"
    );
    assert!(
        !state.battlefield.contains(&bear) && !state.battlefield.contains(&elf),
        "both creatures left the battlefield"
    );
}

#[test]
fn mass_bounce_under_two_redirects_delivers_every_permanent_through_choices() {
    let mut scenario = GameScenario::new();

    // Two independent sources of the same to-hand -> exile redirect.
    // CR 616.1: both are simultaneously applicable to each bounced creature's
    // ZoneChange, so the engine prompts for ordering per creature.
    scenario
        .add_creature(P0, "Redirect A", 0, 0)
        .as_enchantment()
        .with_replacement_definition(to_hand_exile_redirect(
            "If a permanent would be returned to a hand, exile it instead. (A)",
        ));
    scenario
        .add_creature(P0, "Redirect B", 0, 0)
        .as_enchantment()
        .with_replacement_definition(to_hand_exile_redirect(
            "If a permanent would be returned to a hand, exile it instead. (B)",
        ));

    let c0 = scenario.add_creature(P1, "Creature 0", 1, 1).id();
    let c1 = scenario.add_creature(P1, "Creature 1", 1, 1).id();
    let c2 = scenario.add_creature(P1, "Creature 2", 1, 1).id();

    let mut runner = scenario.build();
    let ability = bounce_all_creatures();
    let mut events = Vec::new();
    bounce::resolve_all(runner.state_mut(), &ability, &mut events).unwrap();

    // CR 616.1: each bounced creature surfaces an ordering prompt between the two
    // applicable redirects. Answer each (index 0); bounded so a regression that
    // loops or re-prompts indefinitely fails loudly instead of hanging.
    let mut prompts_answered = 0;
    for _ in 0..20 {
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
    // The discriminating assertions: NO bounced creature may strand on the
    // battlefield; every one of them was redirected to exile.
    for &id in &[c0, c1, c2] {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Exile,
            "every bounced creature must honor the redirect — none may strand on a mid-batch pause"
        );
    }
    assert!(
        !state.battlefield.contains(&c0)
            && !state.battlefield.contains(&c1)
            && !state.battlefield.contains(&c2),
        "all bounced creatures left the battlefield"
    );
    assert!(
        state.players[1].hand.is_empty(),
        "no bounced creature may reach the hand under the redirects"
    );
    assert!(
        state.pending_batch_deliveries.is_none(),
        "the parked mass-bounce tail must be fully drained"
    );
}

//! Phase C3 review fix (round-3, Fix 1) discriminating test: a countered spell
//! whose graveyard move surfaces a CR 616.1 ordering choice must PARK the
//! prompt — not strand the spell as a zone ghost.
//!
//! `zone_pipeline::move_object` did not park `state.waiting_for` on
//! `NeedsChoice` (`replace_event` sets only `pending_replacement`); the C3
//! counter.rs arms bailed with `return Ok(())` under a comment falsely claiming
//! the pipeline parks. Under TWO simultaneously-applicable graveyard→exile
//! redirects (Rest in Peace + Leyline of the Void, or two RIP copies — RIP is
//! not legendary), a countered spell was: removed from the stack,
//! `SpellCountered` emitted, its move parked in `pending_replacement`, and the
//! prompt NEVER surfaced (the engine gates `ChooseReplacement` on the wait
//! state) — permanently ghosting the spell (off `state.stack` but
//! `obj.zone == Stack`).
//!
//! The fix centralizes the park inside the pipeline (`execute_zone_move`'s
//! `replace_event` NeedsChoice arm calls `replacement::park_waiting_for`), so
//! every single-move caller — counter, bounce, and all future C5–C9 migrations
//! — is safe by construction.
//!
//! This test counters a spell under two redirects, asserts the CR 616.1 prompt
//! SURFACES, answers it via a real `GameAction::ChooseReplacement` dispatch, and
//! asserts the spell ends in EXILE with no ghost left behind (consistent zone,
//! `pending_replacement` clear). FAILS pre-fix at the prompt-surfaced assertion
//! (waiting_for never left Priority; the spell ghosted on the Stack zone).

use engine::game::effects::counter;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, ResolvedAbility, TargetFilter,
    TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

/// CR 614.6: "If a card would be put into a graveyard from anywhere, exile it
/// instead." (Rest in Peace / Leyline of the Void class.)
fn graveyard_exile_redirect(description: &str) -> ReplacementDefinition {
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
fn countered_spell_under_two_redirects_surfaces_prompt_and_exiles() {
    let mut scenario = GameScenario::new();

    // Two independent sources of the same graveyard→exile Moved replacement.
    // CR 616.1: both are simultaneously applicable to the countered spell's
    // stack→graveyard ZoneChange, so the engine must prompt for ordering.
    scenario
        .add_creature(P0, "Rest in Peace", 0, 0)
        .as_enchantment()
        .with_replacement_definition(graveyard_exile_redirect(
            "If a card would be put into a graveyard from anywhere, exile it instead. (RIP)",
        ));
    scenario
        .add_creature(P0, "Leyline of the Void", 0, 0)
        .as_enchantment()
        .with_replacement_definition(graveyard_exile_redirect(
            "If a card would be put into a graveyard from anywhere, exile it instead. (Leyline)",
        ));

    let mut runner = scenario.build();

    // P1's spell on the stack, about to be countered.
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(77),
        P1,
        "Doomed Spell".to_string(),
        Zone::Stack,
    );
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(77),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    let ability = ResolvedAbility::new(
        Effect::Counter {
            target: TargetFilter::Any,
            source_rider: None,
            countered_spell_zone: None,
        },
        vec![TargetRef::Object(spell)],
        ObjectId(9999),
        P0,
    );
    let mut events = Vec::new();
    counter::resolve(runner.state_mut(), &ability, &mut events).unwrap();

    // The counter itself is synchronous: the spell left the stack.
    assert!(
        runner.state().stack.is_empty(),
        "the spell must be countered (off the stack)"
    );
    // THE discriminating assertion: the CR 616.1 ordering prompt must SURFACE.
    // Pre-fix the pause was never parked — waiting_for stayed Priority while the
    // move sat in pending_replacement, ghosting the spell on the Stack zone.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the CR 616.1 ordering prompt must be parked in waiting_for \
         (pre-fix: unparked pause, spell ghosted; waiting_for = {:?}, spell zone = {:?})",
        runner.state().waiting_for,
        runner.state().objects[&spell].zone,
    );

    // Answer the prompt through the real engine dispatch path.
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("answer the CR 616.1 ordering prompt");

    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "one choice resolves the single countered spell's ordering race"
    );
    // The redirect delivered: the spell is in exile, not the graveyard, and no
    // ghost remains (zone consistent with container, pending_replacement clear).
    assert_eq!(
        state.objects[&spell].zone,
        Zone::Exile,
        "the countered spell must honor the chosen graveyard->exile redirect"
    );
    assert!(
        state.exile.contains(&spell),
        "exile container must hold the spell (no zone ghost)"
    );
    assert!(
        state.players[1].graveyard.is_empty(),
        "the countered spell must not reach the graveyard under the redirects"
    );
    assert!(
        state.pending_replacement.is_none(),
        "the paused replacement must be fully consumed"
    );
}

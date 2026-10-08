//! Discord thread 1545121827792093275 — `ai-getAction-panic`, 4p Commander vs
//! AI, build v0.71.0 (d3f532c). The reporter's capture panics in
//! `turns::start_next_turn` ("requires an empty stack, no pending resolution
//! carrier, and a settled Priority window") with `stack 0`, `waiting_for
//! Priority`, an `OptionalEffect` frame still on `resolution_stack`, and a
//! triggered `resolving_stack_entry` that can never complete.
//!
//! The capture's frame is Eureka Moment's second sibling ("You may put a land
//! card from your hand onto the battlefield"), and its opponent controls
//! Smothering Tithe. The first sibling draws TWO cards, so Smothering Tithe
//! fires twice and its controller must order the pair (CR 603.3b). That
//! `OrderTriggers` prompt is installed while the spell's own `OptionalEffect`
//! frame is the top resolution frame, so the optional-effect prompt is lost and
//! the frame is never drained.
//!
//! In a debug build the mismatch is caught immediately by
//! `debug_assert_runtime_resolution_invariants`; the shipped WASM is a release
//! build, where the residue survives to the hard `assert!` in
//! `start_next_turn`.
//!
//! The seam under test is the collection-side CR 603.3b guard at the
//! park-vs-process fork in `engine_priority.rs::run_post_action_pipeline`, and
//! the class is every direct-choice resolution frame permitted by
//! `types::resolution::DirectChoiceGate::matches` — not Eureka Moment, which is
//! only the reported instance. `OptionalEffect` and `Proliferate` are the two
//! frame kinds exercised here.
//!
//! Smothering Tithe's Oracle text below is verbatim from Scryfall. The Discord
//! brief and this file's first revision both paraphrased it as "that player
//! creates a Treasure token unless they pay {2}", which inverts the Treasure's
//! controller and routes the engine through an `UnlessPayment` interception the
//! scenario driver cannot answer.
//!
//! The synthesized control card `"One Draw Moment"` — named at each control
//! call site, with its synthesized Oracle text in the one-draw arm of
//! `draw_then_optional_land_scenario`'s `match draws` — is deliberately exempt
//! from that verbatim-Oracle rule: it varies exactly one axis, draw count, and
//! exists to isolate the trigger-count discriminator, so its Oracle text is a
//! controlled variable rather than a card under test.
//!
//! Non-overlap with `raw_resolution_stack_restore.rs`: that module's deferral
//! marker covers a `SpellResolution` frame stranded at the *restore* boundary,
//! a different frame kind and a different cause, and its own doc states that
//! fixing the runtime writer will not move it.

use engine::game::scenario::GameScenario;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const CASTER: PlayerId = PlayerId(0);
const TITHE: PlayerId = PlayerId(1);

const SMOTHERING_TITHE: &str = "Whenever an opponent draws a card, that player may pay {2}. \
                                If the player doesn't, you create a Treasure token.";

/// CR 603.3b + CR 608.2d: triggers that fired during an earlier sibling effect
/// are ordered only after the resolution finishes, so ordering them can never
/// displace the resolving spell's own "may" prompt.
#[test]
fn two_draw_triggers_do_not_strand_the_optional_land_frame() {
    let outcome = cast_draw_then_optional_land("Eureka Moment", 2);
    assert_no_resolution_residue(&outcome);
}

/// Control: one draw fires ONE Smothering Tithe trigger, which needs no APNAP
/// ordering. Passing here while the two-draw case fails isolates the
/// `OrderTriggers` prompt as the discriminator — not "a trigger fired during an
/// earlier sibling effect".
#[test]
fn one_draw_trigger_does_not_strand_the_optional_land_frame() {
    let outcome = cast_draw_then_optional_land("One Draw Moment", 1);
    assert_no_resolution_residue(&outcome);
}

/// Parking is deferral, not dropping: once the optional choice is answered the
/// parked pair reaches the stack and both instances resolve. A fix that
/// silently discarded the batch would still pass the two residue tests above,
/// so this positive delta is what catches it.
///
/// Under Smothering Tithe's real text the drawing player may pay `{2}`; the
/// scenario driver does not pay, so each resolution creates a Treasure for the
/// Tithe's controller (`TITHE`, `PlayerId(1)`) — CR 603.3b's ordering authority
/// is the trigger's controller, not the resolving spell's.
#[test]
fn parked_draw_triggers_reach_the_stack_after_the_optional_choice_is_answered() {
    let outcome = cast_draw_then_optional_land_accepting("Eureka Moment", 2);
    assert_no_resolution_residue(&outcome);
    assert_eq!(
        treasures_controlled_by(&outcome, TITHE),
        2,
        "both parked Smothering Tithe triggers must reach the stack and resolve"
    );
}

/// Control for the row above: one draw fires one trigger and yields one
/// Treasure, so the two-draw arm's `2` is a count delta rather than "Treasures
/// happen at all".
#[test]
fn one_parked_draw_trigger_reaches_the_stack_after_the_optional_choice_is_answered() {
    let outcome = cast_draw_then_optional_land_accepting("One Draw Moment", 1);
    assert_no_resolution_residue(&outcome);
    assert_eq!(treasures_controlled_by(&outcome, TITHE), 1);
}

/// The class is not `OptionalEffect`-specific: a `Proliferate` tail
/// (CR 701.34a) pauses on `WaitingFor::ProliferateChoice` above a
/// `ResolutionFrame::Proliferate`, a second (frame, prompt) pair from the same
/// `DirectChoiceGate::matches` specification, and strands identically.
#[test]
fn two_draw_triggers_do_not_strand_a_proliferate_frame() {
    let outcome = cast_draw_then_proliferate("Eureka Proliferation", 2);
    assert_no_resolution_residue(&outcome);
    assert_eq!(
        outcome.state().resolution_stack.len(),
        1,
        "the row must actually reach the proliferate pause, or it passes vacuously"
    );
}

/// Control on the proliferate axis: one draw fires ONE trigger, which needs no
/// APNAP ordering. It rests at `ProliferateChoice` with a live frame, which is
/// also the row on which `assert_no_resolution_residue`'s `validate` call is an
/// applied check rather than an empty-stack no-op.
#[test]
fn one_draw_trigger_does_not_strand_a_proliferate_frame() {
    let outcome = cast_draw_then_proliferate("One Draw Proliferation", 1);
    assert_no_resolution_residue(&outcome);
    assert_eq!(
        outcome.state().resolution_stack.len(),
        1,
        "the control must actually reach the proliferate pause"
    );
}

/// Hostile — empty/no-choice path. With no Smothering Tithe on the battlefield
/// the draw fires nothing, so the fork's park branch is never taken and the
/// optional prompt is raised and answered normally.
#[test]
fn no_triggers_leaves_the_optional_prompt_untouched() {
    let (mut runner, spell) = draw_then_optional_land_scenario(2, "Eureka Moment", 2, &[]);
    let outcome = runner.cast(spell).resolve();

    assert_no_resolution_residue(&outcome);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "with no observers the resolution must run to a Priority rest, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(treasures_controlled_by(&outcome, TITHE), 0);
}

/// Hostile — multi-authority. Two triggers with DIFFERENT controllers form two
/// singleton CR 603.3b groups, which are auto-ordered and raise no prompt. The
/// fix must change only WHEN ordering is offered, never WHICH batches need it.
///
/// Both triggers resolve, one Treasure to each opponent. A mid-resolution
/// `OrderTriggers` above the live `OptionalEffect` frame would additionally
/// trip `debug_assert_runtime_resolution_invariants` inside `resolve()`.
#[test]
fn two_triggers_from_different_controllers_need_no_ordering() {
    const THIRD: PlayerId = PlayerId(2);

    let (mut runner, spell) =
        draw_then_optional_land_scenario(3, "One Draw Moment", 1, &[TITHE, THIRD]);
    let outcome = runner.cast(spell).accept_optional().resolve();

    assert_no_resolution_residue(&outcome);
    assert_eq!(treasures_controlled_by(&outcome, TITHE), 1);
    assert_eq!(treasures_controlled_by(&outcome, THIRD), 1);
}

fn treasures_controlled_by(
    outcome: &engine::game::scenario::CastOutcome,
    player: PlayerId,
) -> usize {
    outcome
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == Zone::Battlefield && obj.controller == player && obj.name == "Treasure"
        })
        .count()
}

fn cast_draw_then_optional_land(name: &str, draws: u8) -> engine::game::scenario::CastOutcome {
    let (mut runner, spell) = draw_then_optional_land_scenario(2, name, draws, &[TITHE]);
    runner.cast(spell).resolve()
}

/// Same recipe, but the controller accepts the "may" rather than taking the
/// driver's `OptionalPolicy::Decline` default, so the optional prompt is
/// provably still answerable after the trigger batch was parked.
fn cast_draw_then_optional_land_accepting(
    name: &str,
    draws: u8,
) -> engine::game::scenario::CastOutcome {
    let (mut runner, spell) = draw_then_optional_land_scenario(2, name, draws, &[TITHE]);
    runner.cast(spell).accept_optional().resolve()
}

/// Shared `/card-test` recipe for the `OptionalEffect` rows. `tithe_controllers`
/// is the CR 603.3b ordering-authority axis: zero observers (no batch at all),
/// one (a same-controller batch), or two distinct opponents (two singleton
/// groups that need no ordering choice).
fn draw_then_optional_land_scenario(
    players: u8,
    name: &str,
    draws: u8,
    tithe_controllers: &[PlayerId],
) -> (engine::game::scenario::GameRunner, ObjectId) {
    let oracle = match draws {
        1 => "Draw a card. You may put a land card from your hand onto the battlefield.",
        2 => "Draw two cards. You may put a land card from your hand onto the battlefield.",
        _ => unreachable!("only the one- and two-draw arms are modelled"),
    };
    let mut scenario = GameScenario::new_n_player(players, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for controller in tithe_controllers {
        scenario.add_enchantment_from_oracle(*controller, "Smothering Tithe", SMOTHERING_TITHE);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(CASTER, name, false, oracle)
        .id();
    scenario.add_land_to_hand(CASTER, "Forest");
    scenario.with_library_top(CASTER, &["Grizzly Bears", "Runeclaw Bear"]);
    (scenario.build(), spell)
}

/// The `Proliferate` arm of the same class (CR 701.34a). The counter-bearing
/// creature is a load-bearing reach-guard: without it `collect_proliferate_eligible`
/// is empty, the proliferate action completes synchronously and never pushes a
/// resolution frame, and both arms would pass vacuously with an empty stack.
fn cast_draw_then_proliferate(name: &str, draws: u8) -> engine::game::scenario::CastOutcome {
    let oracle = match draws {
        1 => "Draw a card. Proliferate.",
        2 => "Draw two cards. Proliferate.",
        _ => unreachable!("only the one- and two-draw arms are modelled"),
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(TITHE, "Smothering Tithe", SMOTHERING_TITHE);
    scenario
        .add_creature(CASTER, "Counter Bearer", 2, 2)
        .with_plus_counters(1);
    let spell = scenario
        .add_spell_to_hand_from_oracle(CASTER, name, false, oracle)
        .id();
    scenario.with_library_top(CASTER, &["Grizzly Bears", "Runeclaw Bear"]);
    let mut runner = scenario.build();
    runner.cast(spell).resolve()
}

/// A pending resolution frame is only ever legitimate behind the prompt that
/// owns it. Wherever the pipeline comes to rest, the frame stack and the wait
/// must agree — and at a `Priority` rest there must be no residue at all,
/// because `turns::start_next_turn` asserts exactly that.
///
/// Frame/wait agreement is `DirectChoiceGate::matches`, which is private; its
/// public wrapper `ResolutionStack::validate` is the callable authority and is
/// what the engine's own `debug_assert_runtime_resolution_invariants` applies.
///
/// Two honest limits. `validate` early-returns on an empty frame stack, so on
/// the Eureka rows (which rest with `resolution_stack.len() == 0`) it is a
/// documented invariant rather than an applied check; it does real work on the
/// proliferate rows, which rest at `ProliferateChoice` with one live frame. And
/// it runs against the typed `state.resolution_stack` only — the engine's debug
/// assert first calls the `pub(crate)` `canonicalize_legacy_resolution_state`,
/// which additionally merges unmigrated legacy frame families that an
/// integration test cannot reach.
fn assert_no_resolution_residue(outcome: &engine::game::scenario::CastOutcome) {
    let halt = outcome.final_waiting_for().clone();
    let state = outcome.state();
    state
        .resolution_stack
        .validate(&state.waiting_for)
        .unwrap_or_else(|error| {
            panic!("the frame stack and the wait must agree at every rest: {error}")
        });
    if matches!(halt, WaitingFor::Priority { .. }) {
        assert!(
            state.resolution_stack.is_empty() && state.resolving_stack_entry.is_none(),
            "a Priority rest must carry no resolution residue (resolution_stack {}, carrier {})",
            state.resolution_stack.len(),
            state.resolving_stack_entry.is_some(),
        );
    }
}

//! Issue #10431 — a resumed batch member must be classified before its owner drains.
//!
//! `drain_pending_batch_deliveries` requires every resumed member's exact
//! `terminal_completion` (CR 616.1 + CR 603.10a: the look-back trigger record is
//! the settlement authority). A resumed delivery that parks AGAIN in its own tail
//! used to return before the resume seam captured that classification, so the
//! batch owner later drained with `terminal_completion: None` and panicked at
//! `zone_pipeline.rs` ("resumed batch delivery records its exact terminal
//! completion"). The fix records the classification before either terminal.
//!
//! Fixture provenance. Every card with rules text is a real printed card with its
//! verbatim Oracle text, parsed through `*_from_oracle`: Proteus Staff (the
//! activated reveal-until `BatchDelivery` producer), Archelos, Lagoon Mystic and
//! Imposing Sovereign (the CR 616.1 entry tap-state ordering prompt),
//! Deanna Troi, Psychic Counsel, Hardened Scales and Branching Evolution (the
//! CR 616.1 counter ordering prompt). The victim and the revealed entrant are
//! textless filler creatures, which carry no Oracle text to paraphrase. No
//! synthetic card or hand-built replacement definition is used.
//!
//! The ordering prompt that parks the batch (pause 1) needs no controller choice:
//! Imposing Sovereign's "enter tapped" and Archelos's "enter untapped" are both
//! applicable writers of the same entry tap state and do not commute, so the
//! entrant's controller P0 gets a free CR 616.1e choice. This was probed through
//! the public activation before being adopted (it replaced an earlier synthetic
//! `enters_under` pair): the activation reaches pause 1 (Imposing Sovereign vs
//! Archelos), then pause 2 (Hardened Scales vs Branching Evolution), then retires
//! the batch.
//!
//! Imposing Sovereign is used rather than Authority of the Consuls (the same
//! "Creatures your opponents control enter tapped." clause plus a "Whenever a
//! creature an opponent controls enters, you gain 1 life" trigger): Authority was
//! dropped because its trigger leaves one object on the ordinary stack, so the
//! public activation could not end in the empty-stack Priority state that row 1
//! asserts literally. Imposing Sovereign parses to the same entry tap-state
//! replacement and has no trigger.
//!
//! Rows:
//!  1. `proteus_staff_activation_batch_settles_through_tap_then_counter_ordering` —
//!     the production proof. `GameAction::ActivateAbility` -> stack ->
//!     `reveal_until` -> two `ChooseReplacement` answers -> Priority with the
//!     ordinary and resolution stacks empty and the batch retired, no priority pass.
//!  2. `resumed_batch_member_settles_after_tap_state_then_counter_ordering` — seam
//!     row calling `reveal_until::resolve` directly on the same printed board.
//!  3. `resumed_batch_member_without_tail_park_settles_synchronously` — seam
//!     control: the Done path is unchanged (no counter prompt).
//!
//! A planned entering-attacker tail-park row was dropped after measuring that a
//! reveal-until entry with `enters_attacking` reaches `Priority` after the first
//! ordering answer (no `EntryAttackTargetChoice`), so it could not discriminate
//! the seam; the counter tail park above covers the class.

use engine::game::effects::reveal_until;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor, ZoneMoveCompletion};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PROTEUS_STAFF: &str = "{2}{U}, {T}: Put target creature on the bottom of its owner's \
     library. That creature's controller reveals cards from the top of their library until they \
     reveal a creature card. The player puts that card onto the battlefield and the rest on the \
     bottom of their library in any order. Activate only as a sorcery.";
const ARCHELOS_LAGOON_MYSTIC: &str = "As long as Archelos is tapped, other permanents enter \
     tapped.\nAs long as Archelos is untapped, other permanents enter untapped.";
const IMPOSING_SOVEREIGN: &str = "Creatures your opponents control enter tapped.";
const DEANNA_TROI: &str = "Other creatures you control enter with an additional +1/+1 counter on \
     them.";
const HARDENED_SCALES: &str =
    "If one or more +1/+1 counters would be put on a creature you control, \
     that many plus one +1/+1 counters are put on it instead.";
const BRANCHING_EVOLUTION: &str = "If one or more +1/+1 counters would be put on a creature you \
     control, twice that many +1/+1 counters are put on that creature instead.";

/// P0 controls Archelos and Deanna Troi (plus Hardened Scales, and Branching
/// Evolution when `branching_evolution`); P1 controls Imposing Sovereign, which
/// taps creatures P0 puts onto the battlefield.
fn add_printed_board(scenario: &mut GameScenario, branching_evolution: bool) {
    scenario.add_creature_from_oracle(P0, "Archelos, Lagoon Mystic", 2, 4, ARCHELOS_LAGOON_MYSTIC);
    scenario.add_creature_from_oracle(P0, "Deanna Troi, Psychic Counsel", 2, 2, DEANNA_TROI);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
    if branching_evolution {
        scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
    }
    scenario.add_creature_from_oracle(P1, "Imposing Sovereign", 2, 1, IMPOSING_SOVEREIGN);
}

/// Put a textless filler creature card on top of P0's library, with two
/// non-creature cards beneath it.
fn stage_library(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_card_to_library_top(P0, "Below A");
    scenario.add_card_to_library_top(P0, "Below B");
    scenario.add_card_to_library_top(P0, "Textless Filler Creature")
}

/// Give the staged library card its creature type once the runner owns the state.
fn configure_kept_creature(state: &mut GameState, kept: ObjectId) {
    let obj = state.objects.get_mut(&kept).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);
}

fn reveal_until_ability(source: ObjectId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::RevealUntil {
            player: TargetFilter::Controller,
            filter: TargetFilter::Typed(TypedFilter::creature()),
            count: QuantityExpr::Fixed { value: 1 },
            matched_disposition: Default::default(),
            kept_destination: Zone::Battlefield,
            rest_destination: Zone::Library,
            rest_order: Default::default(),
            enter_tapped: Default::default(),
            enters_attacking: false,
            kept_optional_to: None,
            enters_under: None,
            kept_destination_if: None,
        },
        vec![],
        source,
        P0,
    )
}

/// Read the parked batch's paused member classification through the same path
/// the resume capture uses (the batch may sit below a PostReplacement child).
fn parked_terminal_completion(runner: &mut GameRunner) -> Option<Option<ZoneMoveCompletion>> {
    runner
        .state_mut()
        .resolution_stack
        .active_batch_delivery_or_post_replacement_child_mut()
        .and_then(|frame| frame.paused_current.as_ref())
        .map(|paused| paused.terminal_completion)
}

fn describe_waiting(state: &GameState) -> String {
    match &state.waiting_for {
        WaitingFor::ReplacementChoice {
            candidate_count,
            candidates,
            ..
        } => format!(
            "ReplacementChoice({candidate_count}: {})",
            candidates
                .iter()
                .map(|candidate| format!("{}: {}", candidate.source_name, candidate.description))
                .collect::<Vec<_>>()
                .join(" | ")
        ),
        other => format!("{other:?}"),
    }
}

/// Assert the current prompt is a two-way CR 616.1 ordering offered to P0 whose
/// candidates come from `sources`, and return the index of the `answer` source.
fn two_way_ordering_index(state: &GameState, sources: [&str; 2], answer: &str) -> usize {
    match &state.waiting_for {
        WaitingFor::ReplacementChoice {
            player,
            candidate_count: 2,
            candidates,
            ..
        } => {
            assert_eq!(*player, P0, "the entrant's controller orders (CR 616.1)");
            for source in sources {
                assert!(
                    candidates
                        .iter()
                        .any(|candidate| candidate.source_name == source),
                    "{source} must be an offered candidate, got {}",
                    describe_waiting(state)
                );
            }
            candidates
                .iter()
                .position(|candidate| candidate.source_name == answer)
                .unwrap_or_else(|| panic!("{answer} must be offered"))
        }
        other => panic!(
            "expected a two-way ordering of {sources:?}, got {other:?} ({})",
            describe_waiting(state)
        ),
    }
}

/// CR 616.1e + CR 616.1f: answer the entry tap-state ordering with Imposing Sovereign first
/// so Archelos's "untapped" write is applied last and wins.
fn answer_tap_state_ordering(runner: &mut GameRunner) {
    let index = two_way_ordering_index(
        runner.state(),
        ["Imposing Sovereign", "Archelos, Lagoon Mystic"],
        "Imposing Sovereign",
    );
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("answer the tap-state ordering");
}

/// CR 616.1e + CR 122.6: answer the counter ordering with Hardened Scales first.
fn answer_counter_ordering(runner: &mut GameRunner) {
    let index = two_way_ordering_index(
        runner.state(),
        ["Hardened Scales", "Branching Evolution"],
        "Hardened Scales",
    );
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("answer the counter ordering");
}

fn plus_one_counters(state: &GameState, object: ObjectId) -> u32 {
    state
        .objects
        .get(&object)
        .expect("the object is live")
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Production proof. A real activation creates the `BatchDelivery`, the entrant
/// pauses twice (tap-state ordering, then counter ordering), and the resumed
/// member is classified before the batch owner drains.
#[test]
fn proteus_staff_activation_batch_settles_through_tap_then_counter_ordering() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 602.2: {2}{U} for the activation (the {T} is paid by tapping the Staff).
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let staff = scenario
        .add_artifact_from_oracle(P0, "Proteus Staff", PROTEUS_STAFF)
        .id();
    add_printed_board(&mut scenario, true);
    let victim = scenario.add_vanilla(P0, 2, 2);
    let kept = stage_library(&mut scenario);
    let mut runner = scenario.build();
    configure_kept_creature(runner.state_mut(), kept);

    // CR 602.2 + CR 602.5d (sorcery timing, P0's main phase with an empty stack):
    // activate, target the victim, pay, and resolve until the first prompt.
    let outcome = runner.activate(staff, 0).target_object(victim).resolve();

    // Reach guard (CR 616.1): pause 1 is the two-way tap-state ordering, owned by
    // a batch parked by the activation's own resolution.
    two_way_ordering_index(
        outcome.state(),
        ["Imposing Sovereign", "Archelos, Lagoon Mystic"],
        "Imposing Sovereign",
    );
    let state = runner.state();
    assert!(
        state.active_batch_delivery().is_some(),
        "the activation parked a BatchDelivery across pause 1, got {}",
        describe_waiting(state)
    );
    assert!(
        state.objects[&staff].tapped,
        "the Staff's {{T}} cost was paid"
    );
    assert_eq!(
        state.players[0].library.back().copied(),
        Some(victim),
        "the victim went to the bottom of its owner's library"
    );
    assert_eq!(state.objects[&kept].zone, Zone::Library);
    assert_eq!(
        parked_terminal_completion(&mut runner),
        Some(None),
        "nothing is classified before the resumed delivery runs"
    );

    // CR 616.1e + CR 616.1f: Imposing Sovereign first, so Archelos's untap write wins.
    answer_tap_state_ordering(&mut runner);
    // Pause 2: the counter ordering. The resumed segment is already classified
    // (this is the fix) even though its delivery parked again.
    answer_counter_ordering(&mut runner);

    // CR 608.2n + CR 117.3b: with no priority pass in between, the activation's
    // resolution is complete and the active player (P0) receives priority with the batch
    // and every resolution carrier retired.
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "settled back to P0's priority, got {}",
        describe_waiting(state)
    );
    assert!(
        state.active_batch_delivery().is_none() && state.resolution_stack.is_empty(),
        "the batch settled exactly once and no resolution frame survives"
    );
    assert!(state.pending_replacement.is_none());
    // Nothing is left on the ordinary stack: the entrant carries no trigger.
    assert!(
        state.stack.is_empty(),
        "the ordinary stack is empty, got {} entries",
        state.stack.len()
    );
    let entrant = &state.objects[&kept];
    assert_eq!(entrant.zone, Zone::Battlefield, "the kept creature entered");
    assert_eq!(entrant.controller, P0, "the entrant is controlled by P0");
    assert!(
        !entrant.tapped,
        "Archelos's untapped write was applied last (CR 616.1f)"
    );
    // CR 614.1d + CR 122.6: Deanna Troi's one counter, then Hardened Scales first
    // ((1 + 1) x 2 = 4); Branching Evolution first would give 1 x 2 + 1 = 3.
    assert_eq!(
        plus_one_counters(state, kept),
        4,
        "the non-commuting counter replacements apply in the answered order"
    );
}

/// Seam row: the same printed board driven through `reveal_until::resolve`
/// directly. The public activation row above is the production proof.
#[test]
fn resumed_batch_member_settles_after_tap_state_then_counter_ordering() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_printed_board(&mut scenario, true);
    let kept = stage_library(&mut scenario);
    let mut runner = scenario.build();
    configure_kept_creature(runner.state_mut(), kept);

    let ability = reveal_until_ability(kept);
    let mut events = Vec::new();
    reveal_until::resolve(runner.state_mut(), &ability, &mut events).unwrap();

    // Pause 1: Imposing Sovereign vs Archelos (CR 616.1e).
    two_way_ordering_index(
        runner.state(),
        ["Imposing Sovereign", "Archelos, Lagoon Mystic"],
        "Imposing Sovereign",
    );
    assert!(
        runner.state().active_batch_delivery().is_some(),
        "the kept-card batch must be parked across pause 1"
    );
    assert_eq!(
        parked_terminal_completion(&mut runner),
        Some(None),
        "nothing is classified before the resumed delivery runs"
    );
    answer_tap_state_ordering(&mut runner);
    answer_counter_ordering(&mut runner);

    let state = runner.state();
    assert!(
        state.active_batch_delivery().is_none() && state.resolution_stack.is_empty(),
        "the batch must settle exactly once, got {}",
        describe_waiting(state)
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "settled back to priority, got {}",
        describe_waiting(state)
    );
    let entrant = &state.objects[&kept];
    assert_eq!(entrant.zone, Zone::Battlefield, "the kept creature entered");
    assert_eq!(entrant.controller, P0, "the entrant is controlled by P0");
    // CR 614.1d + CR 122.6: (1 from Deanna Troi + 1) x 2 with Hardened Scales first.
    assert_eq!(plus_one_counters(state, kept), 4);
}

/// Seam control: with no second counter replacement the delivery does not park
/// again, so the pre-existing capture path classifies it.
#[test]
fn resumed_batch_member_without_tail_park_settles_synchronously() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Hardened Scales alone is a single counter candidate (no counter ordering).
    add_printed_board(&mut scenario, false);
    let kept = stage_library(&mut scenario);
    let mut runner = scenario.build();
    configure_kept_creature(runner.state_mut(), kept);

    let ability = reveal_until_ability(kept);
    let mut events = Vec::new();
    reveal_until::resolve(runner.state_mut(), &ability, &mut events).unwrap();
    answer_tap_state_ordering(&mut runner);

    let state = runner.state();
    assert!(
        state.active_batch_delivery().is_none(),
        "no tail park: the batch settles synchronously, got {}",
        describe_waiting(state)
    );
    let entrant = &state.objects[&kept];
    assert_eq!(entrant.zone, Zone::Battlefield);
    assert_eq!(entrant.controller, P0);
    // Positive reach guard: Deanna Troi's static applied at runtime (1) plus
    // Hardened Scales (1).
    assert_eq!(
        plus_one_counters(state, kept),
        2,
        "1 from Deanna Troi plus 1 from Hardened Scales"
    );
}

//! Tests for batched triggers with a `valid_card` filter: that
//! `EventContextAmount` resolves to the count of subjects that satisfied the
//! filter (CR 603.2c), not the total event count.
//!
//! Oracle text exercised (The Ur-Dragon, used as a concrete fixture):
//!   Whenever one or more Dragons you control attack, draw that many cards,
//!   then you may put a permanent card from your hand onto the battlefield.
//!
//! Building block under test:
//!   `count_trigger_subjects_in_batch` + `QuantityRef::EventContextAmount` +
//!   the match-count save/restore across `OptionalEffectChoice` round-trips.
//!
//! CR references:
//!   - CR 603.2c: "One or more" (batched) triggers fire once per batch of
//!     simultaneous events; "that many" resolves to the count of subjects
//!     in the batch that satisfied the trigger's `valid_card` filter.
//!   - CR 608.2c: The ability's resolution follows its instructions in the
//!     order written — an optional ("you may") sub-ability must observe the
//!     same trigger context as the pre-pause resolution.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, FilterProp, QuantityExpr, QuantityRef,
    TargetFilter, TriggerDefinition, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::rules::run_combat;

/// Count the cards in P0's hand directly from the state.
fn hand_count(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

/// Build a batched attack trigger that draws cards equal to the number of
/// attacking subjects matching a Dragon filter, then optionally puts a
/// permanent card from hand onto the battlefield.
///
/// CR 603.2c: `batched: true` + `valid_card: Typed{Subtype: Dragon,
/// Controller: You}`. The draw count is `EventContextAmount`, which
/// `stack::resolve_top` lifts from the trigger's filtered subject count.
fn build_batched_attack_trigger() -> TriggerDefinition {
    let permanent_in_hand = TargetFilter::Typed(
        TypedFilter::permanent()
            .controller(ControllerRef::You)
            .properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
    );
    let sub_ability = AbilityDefinition {
        optional: true,
        ..AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: permanent_in_hand,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
            },
        )
    };
    let draw = AbilityDefinition {
        sub_ability: Some(Box::new(sub_ability)),
        ..AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
            },
        )
    };

    let dragon_filter = TargetFilter::Typed(
        TypedFilter::card()
            .controller(ControllerRef::You)
            .subtype("Dragon".to_string()),
    );
    let mut trig = TriggerDefinition::new(TriggerMode::YouAttack)
        .execute(draw)
        .valid_card(dragon_filter)
        .trigger_zones(vec![Zone::Battlefield])
        .description(
            "Whenever one or more Dragons you control attack, draw that many cards, \
             then you may put a permanent card from your hand onto the battlefield."
                .to_string(),
        );
    trig.batched = true;
    trig
}

/// Add the Ur-Dragon fixture (10/10 Dragon Avatar with the batched trigger).
fn add_ur_dragon(scenario: &mut GameScenario) -> ObjectId {
    let mut b = scenario.add_creature(P0, "The Ur-Dragon", 10, 10);
    b.with_subtypes(vec!["Dragon", "Avatar"]);
    b.with_trigger_definition(build_batched_attack_trigger());
    b.id()
}

/// Add a plain Dragon attacker (no trigger of its own).
fn add_dragon(scenario: &mut GameScenario, name: &str) -> ObjectId {
    let mut b = scenario.add_creature(P0, name, 4, 4);
    b.with_subtypes(vec!["Dragon"]);
    b.id()
}

/// Add a non-Dragon attacker to prove the filter excludes non-matching subjects.
fn add_non_dragon(scenario: &mut GameScenario, name: &str) -> ObjectId {
    let mut b = scenario.add_creature(P0, name, 2, 2);
    b.with_subtypes(vec!["Soldier"]);
    b.id()
}

/// Walk the engine until it reaches `WaitingFor::OptionalEffectChoice`,
/// passing priority on each `WaitingFor::Priority` along the way.
fn advance_until_optional_choice(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority should succeed while draining the stack");
            }
            ref other => panic!("unexpected waiting state while draining: {other:?}"),
        }
    }
    panic!("did not reach OptionalEffectChoice within 40 iterations");
}

/// CR 603.2c: When N Dragons attack in a single batch with a non-Dragon
/// attacker mixed in, the trigger fires once and `EventContextAmount` resolves
/// to the filtered subject count (Dragons only), not the total attacker count.
/// Three Dragons + one Soldier → draw exactly 3 cards.
#[test]
fn event_context_amount_matches_filtered_subject_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ur = add_ur_dragon(&mut scenario);
    let d2 = add_dragon(&mut scenario, "Helper Dragon A");
    let d3 = add_dragon(&mut scenario, "Helper Dragon B");
    let non = add_non_dragon(&mut scenario, "Lowly Soldier");
    scenario.with_library_top(P0, &["Top1", "Top2", "Top3", "Top4", "Top5"]);

    let mut runner = scenario.build();
    let hand_before = hand_count(&runner);

    run_combat(&mut runner, vec![ur, d2, d3, non], vec![]);
    advance_until_optional_choice(&mut runner);

    let drawn = hand_count(&runner) - hand_before;
    assert_eq!(
        drawn, 3,
        "CR 603.2c: three Dragons attacked — `EventContextAmount` must resolve to 3, \
         draw 3 cards (non-Dragon attacker must be excluded from count)"
    );
}

/// CR 608.2c: After the Draw resolves, the trigger's resolution is still in
/// flight — the optional `ChangeZone` sub-ability raises an
/// `OptionalEffectChoice` prompt for the controller.
#[test]
fn optional_subability_prompts_after_event_context_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ur = add_ur_dragon(&mut scenario);
    scenario.with_library_top(P0, &["Top1", "Top2"]);

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![ur], vec![]);
    advance_until_optional_choice(&mut runner);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: p, .. } if p == P0
        ),
        "CR 608.2c: optional `ChangeZone` sub-ability of the batched trigger must \
         raise the OptionalEffectChoice prompt for the controller"
    );
}

/// CR 608.2c + CR 603.2c: Accepting the optional "you may" sub-ability prompts
/// the player to choose which permanent card from hand to put onto the
/// battlefield. The `valid_card` filter (`Typed{Permanent}`) must exclude
/// instants from the `EffectZoneChoice` eligible set.
#[test]
fn accepting_optional_subability_filters_eligible_permanents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ur = add_ur_dragon(&mut scenario);

    let creature_in_hand = scenario
        .add_creature_to_hand(P0, "Vanilla Beast", 2, 2)
        .id();
    let enchantment_in_hand = {
        let mut b = scenario.add_creature_to_hand(P0, "Aura In Hand", 0, 0);
        b.as_enchantment();
        b.id()
    };
    let instant_in_hand = {
        let mut b = scenario.add_creature_to_hand(P0, "Lightning Crack", 0, 0);
        b.as_instant();
        b.id()
    };
    scenario.with_library_top(P0, &["Top1", "Top2", "Top3"]);

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![ur], vec![]);
    advance_until_optional_choice(&mut runner);

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("Accept the may sub-ability");

    let waiting = runner.state().waiting_for.clone();
    let WaitingFor::EffectZoneChoice { cards, zone, .. } = waiting else {
        panic!(
            "expected EffectZoneChoice after accepting batched trigger's may sub-ability, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(zone, Zone::Hand, "selection draws from Hand");
    let eligible: std::collections::HashSet<_> = cards.iter().copied().collect();
    assert!(
        eligible.contains(&creature_in_hand),
        "creature in hand is a Permanent card and must be eligible"
    );
    assert!(
        eligible.contains(&enchantment_in_hand),
        "enchantment in hand is a Permanent card and must be eligible"
    );
    assert!(
        !eligible.contains(&instant_in_hand),
        "instant in hand is NOT a Permanent card — must be filtered out"
    );
}

/// CR 608.2c: Selecting a permanent card moves it from Hand to Battlefield,
/// completing the batched trigger's resolution.
#[test]
fn accepting_optional_subability_resolves_zone_move() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ur = add_ur_dragon(&mut scenario);
    let chosen = scenario.add_creature_to_hand(P0, "Chosen Beast", 3, 3).id();
    let _alternate = scenario
        .add_creature_to_hand(P0, "Alternate Beast", 1, 1)
        .id();
    scenario.with_library_top(P0, &["Top1"]);

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![ur], vec![]);
    advance_until_optional_choice(&mut runner);

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("Accept the may sub-ability");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::EffectZoneChoice { .. }
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("Select chosen permanent to put onto battlefield");
    runner.advance_until_stack_empty();

    let zone = runner.state().objects[&chosen].zone;
    assert_eq!(
        zone,
        Zone::Battlefield,
        "CR 608.2c: the chosen permanent must move from Hand to Battlefield"
    );
}

/// CR 608.2c + CR 700.2: Accepting the optional sub-ability when the hand has
/// no eligible permanent short-circuits the inner `ChangeZone` (empty eligible
/// set → no `EffectZoneChoice`), the trigger resolution completes, and the
/// engine returns to Priority.
#[test]
fn accepting_optional_subability_with_no_eligible_permanents_short_circuits() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ur = add_ur_dragon(&mut scenario);
    let instant_id = {
        let mut b = scenario.add_creature_to_hand(P0, "Lightning Crack", 0, 0);
        b.as_instant();
        b.id()
    };
    scenario.with_library_top(P0, &["Top1"]);

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![ur], vec![]);
    advance_until_optional_choice(&mut runner);

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("Accept the may sub-ability with no eligible permanents");
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "accept-with-no-targets must NOT raise EffectZoneChoice — empty \
         eligible set short-circuits the sub-ability and resolution completes"
    );
    assert_eq!(
        runner.state().objects[&instant_id].zone,
        Zone::Hand,
        "the instant remains in hand (it was never eligible)"
    );
}

/// CR 608.2c: Declining the optional sub-ability finishes resolution and
/// returns control to Priority. The drawn cards remain in hand; no permanent
/// is put onto the battlefield.
#[test]
fn declining_optional_subability_returns_to_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ur = add_ur_dragon(&mut scenario);
    let unmoved = scenario.add_creature_to_hand(P0, "Stays Home", 1, 1).id();
    scenario.with_library_top(P0, &["Top1"]);

    let mut runner = scenario.build();
    let hand_before = hand_count(&runner);
    run_combat(&mut runner, vec![ur], vec![]);
    advance_until_optional_choice(&mut runner);

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("Decline the may sub-ability");
    runner.advance_until_stack_empty();

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        runner.state().objects[&unmoved].zone,
        Zone::Hand,
        "declined sub-ability leaves the in-hand permanent in place"
    );
    let drawn = hand_count(&runner) - hand_before;
    assert_eq!(
        drawn, 1,
        "the Draw clause still resolved (1 Dragon attacked → 1 card drawn)"
    );
}

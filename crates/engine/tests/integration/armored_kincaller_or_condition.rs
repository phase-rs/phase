//! Armored Kincaller — `Or`-wrapped performed-gate on an optional ETB trigger.
//!
//! Oracle: "When this creature enters, you may reveal a Dinosaur card from your
//! hand. If you do or if you control another Dinosaur, you gain 3 life."
//!
//! The ETB trigger is `optional` ("you may reveal"); its `GainLife` sub-ability
//! carries the composite condition `Or { [IfYouDo, QuantityCheck(control
//! another Dinosaur)] }`. When the controller DECLINES the optional reveal but
//! controls another Dinosaur, the `Or`'s second disjunct is satisfied, so they
//! must still gain 3 life.
//!
//! Before the fix, `should_resolve_subability_on_optional_decline` classified
//! every `Or`/`And` condition as "not a decline branch" — so on decline the
//! `GainLife` sub-ability was dropped entirely and the player never gained
//! life, even though `QuantityCheck` was true.
//!
//! This drives the real cast -> stack -> ETB trigger -> `OptionalEffectChoice`
//! -> `DecideOptionalEffect { accept: false }` pipeline through `apply`.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 603.3: once an ability triggers, its controller puts it on the stack.
//!   - CR 608.2c: "the controller follows the ability's instructions in the
//!     order written"; a condition that gates an instruction is evaluated as
//!     the ability resolves.
//!   - CR 608.2d: an effect offering a choice (here the optional "you may
//!     reveal") has the player announce it while applying the effect.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

/// Armored Kincaller's printed Oracle text — byte-identical to
/// `client/public/card-data.json`.
const ARMORED_KINCALLER: &str = "When this creature enters, you may reveal a \
Dinosaur card from your hand. If you do or if you control another Dinosaur, \
you gain 3 life.";

fn life(
    runner: &engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.life)
        .expect("player exists")
}

/// Cast a 0-cost creature from hand and resolve its ETB trigger up to the
/// `OptionalEffectChoice` prompt. The `RevealHand` head targets the player
/// whose hand is revealed ("your hand"); `TriggerTargetSelection` surfaces
/// first — `choose_first_legal_target` binds the controller (P0) before the
/// optional "you may reveal" prompt appears.
fn cast_creature_and_reach_optional_prompt(
    runner: &mut engine::game::scenario::GameRunner,
    hand_card: ObjectId,
) {
    let card_id = runner
        .state()
        .objects
        .get(&hand_card)
        .expect("hand card exists")
        .card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: hand_card,
            card_id,
            targets: vec![],
        })
        .expect("casting a 0-cost creature should succeed");
    runner.advance_until_stack_empty();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        runner
            .choose_first_legal_target()
            .expect("binding the RevealHand player target must succeed");
        runner.advance_until_stack_empty();
    }
}

/// Core fix: decline the optional reveal, but control another Dinosaur — the
/// `Or` condition's `QuantityCheck` disjunct is true, so the controller gains
/// 3 life.
#[test]
fn declined_reveal_still_gains_life_when_controlling_another_dinosaur() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    // Another Dinosaur already on the battlefield satisfies the `QuantityCheck`
    // disjunct of the `Or` condition.
    scenario
        .add_creature(P0, "Ranging Raptors", 2, 4)
        .with_subtypes(vec!["Dinosaur"]);

    let kincaller = scenario
        .add_creature_to_hand_from_oracle(P0, "Armored Kincaller", 1, 5, ARMORED_KINCALLER)
        .with_subtypes(vec!["Dinosaur"])
        .id();

    let mut runner = scenario.build();
    cast_creature_and_reach_optional_prompt(&mut runner, kincaller);

    // The optional "you may reveal" head should have surfaced an
    // `OptionalEffectChoice` once the ETB trigger resolved.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "expected OptionalEffectChoice, got {:?}",
        runner.state().waiting_for
    );

    let life_before = life(&runner, P0);

    // Decline the optional reveal.
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the optional reveal must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        life(&runner, P0),
        life_before + 3,
        "declining the reveal must still gain 3 life — the `Or` condition's \
         'control another Dinosaur' disjunct is satisfied"
    );
}

/// Control check: decline the optional reveal with NO other Dinosaur on the
/// battlefield — both `Or` disjuncts are false, so no life is gained.
#[test]
fn declined_reveal_no_dinosaur_gains_no_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    let kincaller = scenario
        .add_creature_to_hand_from_oracle(P0, "Armored Kincaller", 1, 5, ARMORED_KINCALLER)
        .with_subtypes(vec!["Dinosaur"])
        .id();

    let mut runner = scenario.build();
    cast_creature_and_reach_optional_prompt(&mut runner, kincaller);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "expected OptionalEffectChoice, got {:?}",
        runner.state().waiting_for
    );

    let life_before = life(&runner, P0);

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the optional reveal must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        life(&runner, P0),
        life_before,
        "declining the reveal with no other Dinosaur gains no life — both \
         `Or` disjuncts are false"
    );
}

//! CR 614.1a + CR 606.4: Actor-scoped counter replacements (Vorinclex,
//! Monstrous Raider / Halving Season) applied to a planeswalker's loyalty
//! activation, driven through the real `apply()` pipeline.
//!
//! Both Vorinclex clauses are constructed test-locally (the parser fix that
//! marks them `CounterReplacementSubject::Actor` is inert until card data is
//! regenerated, so these tests exercise the runtime scoping + the
//! finalize/complete loyalty-activation refactor, not the parser).
//!
//! NOTE on discrimination: for a *loyalty* ability the actor (the activating
//! player) is always the recipient planeswalker's controller (CR 606.3), so
//! `Actor` and `Recipient` subjects coincide here. These tests therefore prove
//! the loyalty pipeline resolves a single competing counter replacement
//! correctly and the finalize→complete refactor's `Paid` path is intact; the
//! actor-vs-recipient axis itself is discriminated where actor != recipient in
//! `effects::counters::tests::actor_scoped_doubler_applies_when_actor_differs_from_recipient`.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CounterReplacementSubject, Effect, QuantityExpr,
    QuantityModification, ReplacementDefinition, ReplacementPlayerScope, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;

/// A non-targeted `+amount` loyalty ability (draws a card on resolution, but the
/// ability just sits on the stack in these tests — only the loyalty cost fires).
fn plus_loyalty_ability(amount: i32) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Loyalty { amount })
    .sorcery_speed()
}

/// Vorinclex's "If you would put …, put twice that many" doubling clause.
fn vorinclex_doubling() -> ReplacementDefinition {
    let mut def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
        .valid_card(TargetFilter::Any)
        .quantity_modification(QuantityModification::DOUBLE)
        .counter_subject(CounterReplacementSubject::Actor)
        .description("double".to_string());
    def.valid_player = Some(ReplacementPlayerScope::You);
    def
}

/// Vorinclex's "If an opponent would put …, they put half that many" clause.
fn vorinclex_halving() -> ReplacementDefinition {
    let mut def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
        .valid_card(TargetFilter::Any)
        .quantity_modification(QuantityModification::Half)
        .counter_subject(CounterReplacementSubject::Actor)
        .description("halve".to_string());
    def.valid_player = Some(ReplacementPlayerScope::Opponent);
    def
}

/// Test A: an opponent's Vorinclex halves the loyalty counters a player adds:
/// `+2` becomes `+1`. The doubling clause is present but must not fire (P0 is
/// not "you" relative to P1's Vorinclex), so the final delta is `+1`, not
/// `+2`/`+4`.
#[test]
fn opponent_vorinclex_halves_own_loyalty_activation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0's planeswalker with a single `+2` loyalty ability.
    let pw = scenario
        .add_creature(P0, "Test Walker", 0, 0)
        .as_planeswalker_with_loyalty("Test", 5)
        .with_ability_definition(plus_loyalty_ability(2))
        .id();

    // P1 (opponent) controls Vorinclex carrying BOTH clauses.
    scenario
        .add_creature(P1, "Vorinclex, Monstrous Raider", 6, 6)
        .with_replacement_definition(vorinclex_doubling())
        .with_replacement_definition(vorinclex_halving());

    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: pw,
            ability_index: 0,
        })
        .expect("activate the +2 loyalty ability");

    let state = runner.state();
    // Halving fired (opponent's Vorinclex), doubling did not: 5 + floor(2/2) = 6.
    assert_eq!(
        state.objects[&pw].loyalty,
        Some(6),
        "opponent's Vorinclex must halve +2 → +1 (doubling present but must not fire)"
    );
    assert_eq!(
        state.objects[&pw]
            .counters
            .get(&CounterType::Loyalty)
            .copied(),
        Some(6)
    );
    // No CR 616.1 prompt: only one replacement is applicable.
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "a single applicable replacement must not surface an ordering prompt: {:?}",
        state.waiting_for
    );
    // The activated ability is on the stack.
    assert!(
        matches!(
            state.stack.back().map(|e| &e.kind),
            Some(StackEntryKind::ActivatedAbility { .. })
        ),
        "the loyalty ability must be on the stack"
    );
    assert_eq!(state.objects[&pw].loyalty_activations_this_turn, 1);
    assert_eq!(
        state
            .loyalty_abilities_activated_this_turn
            .get(&P0)
            .copied(),
        Some(1)
    );
}

/// Test B (negative sibling to A): Vorinclex's own controller activates a `+2`
/// loyalty ability, and Vorinclex doubles it to `+4`. Halving is present but
/// must not fire (its controller is not their own opponent). Contrast with
/// Test A: the same activation diverges (+1 vs +4) purely on which clause the
/// actor satisfies.
#[test]
fn own_vorinclex_doubles_own_loyalty_activation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let pw = scenario
        .add_creature(P0, "Test Walker", 0, 0)
        .as_planeswalker_with_loyalty("Test", 5)
        .with_ability_definition(plus_loyalty_ability(2))
        .id();

    // P0 controls both the planeswalker AND Vorinclex.
    scenario
        .add_creature(P0, "Vorinclex, Monstrous Raider", 6, 6)
        .with_replacement_definition(vorinclex_doubling())
        .with_replacement_definition(vorinclex_halving());

    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: pw,
            ability_index: 0,
        })
        .expect("activate the +2 loyalty ability");

    let state = runner.state();
    // Doubling fired (your own Vorinclex), halving did not: 5 + 2*2 = 9.
    assert_eq!(
        state.objects[&pw].loyalty,
        Some(9),
        "your own Vorinclex must double +2 → +4 (halving present but must not fire)"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "a single applicable replacement must not surface an ordering prompt: {:?}",
        state.waiting_for
    );
    assert!(matches!(
        state.stack.back().map(|e| &e.kind),
        Some(StackEntryKind::ActivatedAbility { .. })
    ));
    assert_eq!(state.objects[&pw].loyalty_activations_this_turn, 1);
}

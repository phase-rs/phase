//! Production-path coverage for Imperial Subduer's targeted attacks-alone trigger.
//!
//! The fixture uses the card's exact Oracle text and exercises both qualifying
//! subtypes, declaration-time sole-attacker evaluation, targeted-trigger choice,
//! same-controller trigger ordering, and a hostile post-declaration Mobilize
//! mutation of the live attacker set.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{TargetRef, TriggerCondition};
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

use super::rules::AttackTarget;

const IMPERIAL_SUBDUER_ORACLE: &str =
    "Whenever a Samurai or Warrior you control attacks alone, tap target creature you don't control.";

const REIGNING_VICTOR_ORACLE: &str = "Mobilize 1 (Whenever this creature attacks, create a tapped and attacking 1/1 red Warrior creature token. Sacrifice it at the beginning of the next end step.)\n\
When this creature enters, target creature gets +1/+0 and gains indestructible until end of turn. (Damage and effects that say \"destroy\" don't destroy it.)";

const MOBILIZE_TRIGGER_DESCRIPTION: &str = "Mobilize — create Warrior tokens tapped and attacking";

fn add_imperial_subduer(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, "Imperial Subduer", 3, 2)
        .with_subtypes(vec!["Human", "Samurai"])
        .from_oracle_text(IMPERIAL_SUBDUER_ORACLE)
        .id()
}

fn add_reigning_victor(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, "Reigning Victor", 3, 3)
        .with_subtypes(vec!["Orc", "Warrior"])
        .from_oracle_text_with_keywords(&["Mobilize"], REIGNING_VICTOR_ORACLE)
        .id()
}

fn assert_imperial_trigger_is_installed(runner: &GameRunner, imperial: ObjectId) {
    assert!(
        runner.state().objects[&imperial]
            .trigger_definitions
            .iter_unchecked()
            .any(|entry| {
                entry.definition.mode == TriggerMode::Attacks
                    && entry.definition.description.as_deref() == Some(IMPERIAL_SUBDUER_ORACLE)
            }),
        "reach guard: exact Imperial Subduer Oracle must install its attack trigger"
    );
}

fn declared_attackers(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .combat
        .as_ref()
        .expect("combat must remain live")
        .attackers
        .iter()
        .map(|attacker| attacker.object_id)
        .collect()
}

/// The outer `Option` distinguishes an absent stack entry from a present
/// triggered ability whose declaration-time condition has been consumed.
fn stack_condition_for_source(
    runner: &GameRunner,
    source_id: ObjectId,
) -> Option<Option<TriggerCondition>> {
    runner.state().stack.iter().find_map(|entry| {
        if entry.source_id != source_id {
            return None;
        }
        match &entry.kind {
            StackEntryKind::TriggeredAbility { condition, .. } => Some(condition.clone()),
            _ => None,
        }
    })
}

fn choose_imperial_target(
    runner: &mut GameRunner,
    chosen: ObjectId,
    other_legal_target: ObjectId,
    controller_owned_illegal_target: ObjectId,
) {
    let WaitingFor::TriggerTargetSelection {
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected Imperial Subduer's TriggerTargetSelection, got {:?}",
            runner.state().waiting_for
        );
    };
    let slot = &target_slots[selection.current_slot];
    let chosen_ref = TargetRef::Object(chosen);
    assert!(
        slot.legal_targets.contains(&chosen_ref),
        "chosen opposing creature must be legal"
    );
    assert!(
        slot.legal_targets
            .contains(&TargetRef::Object(other_legal_target)),
        "the second opposing creature keeps target identity observable"
    );
    assert!(
        !slot
            .legal_targets
            .contains(&TargetRef::Object(controller_owned_illegal_target)),
        "a creature controlled by Imperial Subduer's controller must be illegal"
    );

    // CR 603.3d: A targeted triggered ability chooses its target while it is
    // being put on the stack.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(chosen_ref),
        })
        .expect("choosing Imperial Subduer's target should succeed");
}

fn order_imperial_below_mobilize(runner: &mut GameRunner, imperial: ObjectId, mobilizer: ObjectId) {
    let WaitingFor::OrderTriggers { player, triggers } = runner.state().waiting_for.clone() else {
        panic!(
            "expected Imperial Subduer and Mobilize to require ordering, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "P0 controls both attack triggers");
    assert_eq!(triggers.len(), 2, "exactly two attack triggers must fire");

    let imperial_index = triggers
        .iter()
        .position(|summary| {
            summary.source_id == imperial && summary.description == IMPERIAL_SUBDUER_ORACLE
        })
        .expect("ordering prompt must identify Imperial Subduer's trigger");
    let mobilize_index = triggers
        .iter()
        .position(|summary| {
            summary.source_id == mobilizer && summary.description == MOBILIZE_TRIGGER_DESCRIPTION
        })
        .expect("ordering prompt must identify Reigning Victor's Mobilize trigger");
    assert_ne!(
        imperial_index, mobilize_index,
        "the two trigger identities must occupy distinct prompt positions"
    );

    // CR 603.3b: P0 puts its simultaneous triggers on the stack in the chosen
    // order. Output position zero is the bottom, so Mobilize will resolve first.
    runner
        .act(GameAction::OrderTriggers {
            order: vec![imperial_index, mobilize_index],
        })
        .expect("ordering Imperial Subduer below Mobilize should succeed");
}

#[test]
fn samurai_attacks_alone_taps_exact_target_and_stores_no_condition() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let imperial = add_imperial_subduer(&mut scenario);
    let samurai = scenario
        .add_creature(P0, "Test Samurai", 2, 2)
        .with_subtypes(vec!["Samurai"])
        .id();
    let chosen = scenario.add_creature(P1, "Chosen Defender", 2, 2).id();
    let unchosen = scenario.add_creature(P1, "Unchosen Defender", 2, 2).id();
    let friendly = scenario.add_creature(P0, "Friendly Creature", 2, 2).id();
    let mut runner = scenario.build();

    assert_imperial_trigger_is_installed(&runner, imperial);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(samurai, AttackTarget::Player(P1))])
        .expect("the Samurai must be a legal lone attacker");
    choose_imperial_target(&mut runner, chosen, unchosen, friendly);

    // CR 506.5 + CR 508.1m: the sole-attacker fact is consumed when attackers
    // are declared; it is not a condition to recheck during resolution.
    assert_eq!(
        stack_condition_for_source(&runner, imperial),
        Some(None),
        "Imperial's trigger must be present with no stored condition"
    );

    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&chosen].tapped,
        "the exact chosen opposing creature must be tapped"
    );
    assert!(
        !runner.state().objects[&unchosen].tapped,
        "the other legal target must remain untapped"
    );
}

#[test]
fn warrior_mobilize_mutates_attackers_before_imperial_trigger_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let imperial = add_imperial_subduer(&mut scenario);
    let mobilizer = add_reigning_victor(&mut scenario);
    let chosen = scenario.add_creature(P1, "Chosen Defender", 2, 2).id();
    let unchosen = scenario.add_creature(P1, "Unchosen Defender", 2, 2).id();
    let mut runner = scenario.build();

    assert_imperial_trigger_is_installed(&runner, imperial);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(mobilizer, AttackTarget::Player(P1))])
        .expect("Reigning Victor must be a legal lone Warrior attacker");
    order_imperial_below_mobilize(&mut runner, imperial, mobilizer);
    choose_imperial_target(&mut runner, chosen, unchosen, imperial);

    assert_eq!(
        stack_condition_for_source(&runner, imperial),
        Some(None),
        "Imperial's declaration-time attacks-alone gate must not remain stored"
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .last()
            .map(|entry| entry.source_id),
        Some(mobilizer),
        "Mobilize must be the top stack entry before resolution"
    );

    runner.resolve_top();

    // CR 702.181a + CR 508.4: Mobilize creates one tapped and attacking
    // Warrior, but that token was not declared as an attacker.
    let tokens: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.controller == P0 && object.is_token && object.name == "Warrior")
        .map(|object| object.id)
        .collect();
    assert_eq!(
        tokens.len(),
        1,
        "Mobilize 1 must create exactly one Warrior"
    );
    assert!(
        runner.state().objects[&tokens[0]].tapped,
        "the Mobilize token must enter tapped"
    );
    let attackers = declared_attackers(&runner);
    assert_eq!(
        attackers.len(),
        2,
        "Mobilize must add a second creature to the live attacker set"
    );
    assert!(
        attackers.contains(&mobilizer) && attackers.contains(&tokens[0]),
        "the live attacker set must contain Reigning Victor and its Mobilize token"
    );
    assert!(
        !runner.state().objects[&chosen].tapped,
        "Imperial must still be pending after Mobilize resolves"
    );
    assert_eq!(
        stack_condition_for_source(&runner, imperial),
        Some(None),
        "the pending Imperial trigger must remain unconditional after mutation"
    );

    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&chosen].tapped,
        "Imperial must tap its chosen target despite the later second attacker"
    );
    assert!(
        !runner.state().objects[&unchosen].tapped,
        "Imperial must preserve the unchosen target's identity"
    );
}

#[test]
fn samurai_with_coattacker_does_not_trigger_imperial_subduer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let imperial = add_imperial_subduer(&mut scenario);
    let samurai = scenario
        .add_creature(P0, "Test Samurai", 2, 2)
        .with_subtypes(vec!["Samurai"])
        .id();
    let coattacker = scenario.add_creature(P0, "Companion", 2, 2).id();
    let defender = scenario.add_creature(P1, "Defender", 2, 2).id();
    let mut runner = scenario.build();

    assert_imperial_trigger_is_installed(&runner, imperial);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (samurai, AttackTarget::Player(P1)),
            (coattacker, AttackTarget::Player(P1)),
        ])
        .expect("both creatures must be legal attackers");

    // CR 506.5: the live combat state proves the Samurai has a co-attacker, so
    // this negative assertion reached the sole-attacker gate.
    assert_eq!(
        declared_attackers(&runner),
        vec![samurai, coattacker],
        "reach guard: both declared attackers must be recorded"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "a rejected attacks-alone trigger must not request a target"
    );
    assert_eq!(
        stack_condition_for_source(&runner, imperial),
        None,
        "Imperial must not trigger when the Samurai has a co-attacker"
    );
    assert!(
        !runner.state().objects[&defender].tapped,
        "the opposing creature must remain untapped"
    );
}

#[test]
fn lone_non_samurai_non_warrior_does_not_trigger_imperial_subduer() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let imperial = add_imperial_subduer(&mut scenario);
    let bear = scenario
        .add_creature(P0, "Runeclaw Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let defender = scenario.add_creature(P1, "Defender", 2, 2).id();
    let mut runner = scenario.build();

    assert_imperial_trigger_is_installed(&runner, imperial);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bear, AttackTarget::Player(P1))])
        .expect("the Bear must be a legal lone attacker");

    // CR 506.5: the exact one-attacker combat state proves the negative reaches
    // Imperial's Samurai-or-Warrior subject filter rather than failing earlier.
    assert_eq!(
        declared_attackers(&runner),
        vec![bear],
        "reach guard: the Bear must be the sole declared attacker"
    );
    let bear_subtypes = &runner.state().objects[&bear].card_types.subtypes;
    assert!(
        bear_subtypes.len() == 1 && bear_subtypes[0] == "Bear",
        "reach guard: the lone attacker is neither a Samurai nor a Warrior"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "a subtype-rejected trigger must not request a target"
    );
    assert_eq!(
        stack_condition_for_source(&runner, imperial),
        None,
        "Imperial must not trigger for the lone Bear"
    );
    assert!(
        !runner.state().objects[&defender].tapped,
        "the opposing creature must remain untapped"
    );
}

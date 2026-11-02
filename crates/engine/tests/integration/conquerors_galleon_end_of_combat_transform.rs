//! CR 603.7a + CR 608.2c: a "then" continuation of a suffix-timed delayed
//! instruction belongs to the same delayed payload.
//!
//! Conqueror's Galleon: "When this Vehicle attacks, exile it at end of combat,
//! then return it to the battlefield transformed under your control." The
//! reported bug: the Galleon was exiled at end of combat but never came back.
//! The "then return it …" clause was emitted as a sibling of the delayed
//! trigger, so it resolved with the attack trigger — before the exile it
//! follows — found nothing to return, and the end-of-combat exile stranded the
//! card.

use super::rules::{GameScenario, Phase, P0, P1};
use engine::game::combat::AttackTarget;
use engine::game::game_object::BackFaceData;
use engine::game::scenario::GameRunner;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::mana::ManaCost;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall) of the front face's triggered ability.
const GALLEON_TRIGGER: &str = "When this Vehicle attacks, exile it at end of combat, then return it to the battlefield transformed under your control.";

fn foothold_back_face() -> BackFaceData {
    BackFaceData {
        is_swap_snapshot: false,
        trigger_printed_origins: Vec::new(),
        name: "Conqueror's Foothold".to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Land],
            subtypes: vec![],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
        parse_warnings: vec![],
    }
}

/// Pass priority (resolving whatever is on the stack) until `phase` begins.
fn pass_priority_until(runner: &mut GameRunner, phase: Phase) {
    for _ in 0..16 {
        if runner.state().phase == phase {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority through combat");
    }
    panic!("never reached {phase:?}");
}

/// CR 603.7a: the return is nested inside the delayed payload, after the exile,
/// not emitted as a sibling of the `CreateDelayedTrigger` wrapper.
#[test]
fn then_continuation_nests_in_suffix_delayed_payload() {
    let execute = parse_effect_chain(
        "exile it at end of combat, then return it to the battlefield transformed under your control.",
        AbilityKind::Spell,
    );
    let Effect::CreateDelayedTrigger { effect, .. } = &*execute.effect else {
        panic!("expected a delayed-trigger head, got {:?}", execute.effect);
    };
    assert!(
        execute.sub_ability.is_none(),
        "the return must not resolve alongside the trigger: {:?}",
        execute.sub_ability
    );
    assert!(
        matches!(
            &*effect.effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        ),
        "payload head must be the exile, got {:?}",
        effect.effect
    );
    let ret = effect
        .sub_ability
        .as_deref()
        .expect("return must continue the delayed payload");
    assert!(
        matches!(
            &*ret.effect,
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                enter_transformed: true,
                ..
            }
        ),
        "payload continuation must be the transformed return, got {:?}",
        ret.effect
    );
}

/// Reach guard: the chain must reach the delayed-payload fold intact — a
/// delayed-trigger head and no unimplemented node anywhere.
fn delayed_payload(execute: &AbilityDefinition) -> &AbilityDefinition {
    assert!(
        !format!("{execute:?}").contains("Unimplemented"),
        "the chain must parse fully: {execute:?}"
    );
    let Effect::CreateDelayedTrigger { effect, .. } = &*execute.effect else {
        panic!("expected a delayed-trigger head, got {:?}", execute.effect);
    };
    effect
}

/// CR 608.2c: a new sentence after a suffix-timed instruction is its own
/// instruction — it resolves now, not inside the delayed payload.
#[test]
fn sentence_after_suffix_delayed_instruction_stays_sibling() {
    let execute = parse_effect_chain(
        "Return it to the battlefield under its owner's control at the beginning of the next end step. You gain 2 life.",
        AbilityKind::Spell,
    );
    let payload = delayed_payload(&execute);
    assert!(
        payload.sub_ability.is_none(),
        "the life gain must not join the payload: {:?}",
        payload.sub_ability
    );
    let sibling = execute
        .sub_ability
        .as_deref()
        .expect("the life gain resolves alongside the trigger");
    assert!(
        matches!(&*sibling.effect, Effect::GainLife { .. }),
        "expected the life gain sibling, got {:?}",
        sibling.effect
    );
}

/// CR 608.2c: "... at the beginning of the next end step and you skip your
/// next draw step" (Ivory Gargoyle) — the skip is a separate instruction, not
/// part of the delayed return.
#[test]
fn and_joined_instruction_after_suffix_delayed_instruction_stays_sibling() {
    let execute = parse_effect_chain(
        "return it to the battlefield under its owner's control at the beginning of the next end step and you skip your next draw step.",
        AbilityKind::Spell,
    );
    let payload = delayed_payload(&execute);
    assert!(
        matches!(
            &*payload.effect,
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                ..
            }
        ),
        "payload head must be the return, got {:?}",
        payload.effect
    );
    assert!(
        payload.sub_ability.is_none(),
        "the skip must not join the payload: {:?}",
        payload.sub_ability
    );
    assert!(
        execute.sub_ability.is_some(),
        "the skip resolves alongside the trigger"
    );
}

/// CR 508.1m + CR 511.2 + CR 712.14a: the Galleon attacks, stays on the
/// battlefield through combat, is exiled at end of combat, and returns with its
/// back face up — a new, untapped object that isn't attacking.
#[test]
fn conquerors_galleon_returns_transformed_after_end_of_combat_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // A crewed Vehicle: an artifact creature until end of turn.
    let galleon = {
        let mut b = scenario.add_creature(P0, "Conqueror's Galleon", 2, 10);
        b.as_artifact_creature();
        b.with_subtypes(vec!["Vehicle"]);
        b.from_oracle_text(GALLEON_TRIGGER);
        b.id()
    };
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&galleon)
        .unwrap()
        .back_face = Some(foothold_back_face());

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(galleon, AttackTarget::Player(P1))])
        .expect("declare attackers");
    runner.advance_until_stack_empty();

    let after_trigger = &runner.state().objects[&galleon];
    assert_eq!(
        after_trigger.zone,
        Zone::Battlefield,
        "CR 603.7a: the exile waits for end of combat"
    );
    assert!(
        !after_trigger.transformed,
        "the return must not resolve with the attack trigger"
    );

    // CR 511.2: the delayed trigger fires as the end of combat step begins.
    pass_priority_until(&mut runner, Phase::PostCombatMain);

    let foothold = &runner.state().objects[&galleon];
    assert_eq!(
        foothold.zone,
        Zone::Battlefield,
        "the Galleon must return after its end-of-combat exile"
    );
    assert!(
        foothold.transformed,
        "CR 712.14a: it returns with its back face up"
    );
    assert_eq!(foothold.name, "Conqueror's Foothold");
    assert!(
        foothold.card_types.core_types.contains(&CoreType::Land),
        "Conqueror's Foothold is a land"
    );
    // The attacking (tapped) Galleon would still be tapped had it transformed
    // in place; untapped proves it was exiled and returned.
    assert!(
        !foothold.tapped,
        "CR 400.7 + CR 110.5b: the returned permanent is a new object and enters untapped"
    );
    assert_eq!(foothold.controller, P0);
}

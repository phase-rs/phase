//! Production-path regressions for #8089's two reported copy cards.

use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::turns::execute_cleanup;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FACETAKER_ORACLE: &str = "This creature can't be blocked.\nAt the beginning of combat on your turn, you may have this creature become a copy of another target creature until end of turn, except it's 1/4 and has \"This creature can't be blocked.\"";
const GLASSPOOL_ORACLE: &str = "You may have this creature enter as a copy of a creature you control, except it's a Shapeshifter Rogue in addition to its other types.";

fn resolve_facetaker_combat_trigger(runner: &mut GameRunner, target: ObjectId) {
    // Advance through the beginning-of-combat priority window so the printed
    // trigger is actually put on the stack; merely arriving at BeginCombat
    // leaves that window unopened.
    runner.advance_to_phase(Phase::DeclareAttackers);

    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => runner.advance_until_stack_empty(),
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("Facetaker's trigger must accept its other-creature target");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept Facetaker's optional copy");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while Facetaker's trigger resolves");
            }
            other => panic!("unexpected Facetaker trigger prompt: {other:?}"),
        }
    }
    panic!("Facetaker trigger did not settle");
}

#[test]
fn cephalid_facetaker_copy_expires_at_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P0, "Facetaker Target", 5, 5).id();
    let facetaker = scenario
        .add_creature_from_oracle(P0, "Cephalid Facetaker", 1, 4, FACETAKER_ORACLE)
        .id();
    let mut runner = scenario.build();

    resolve_facetaker_combat_trigger(&mut runner, target);
    assert_eq!(runner.state().objects[&facetaker].name, "Facetaker Target");

    // CR 707.9b: the copy exception's bare-P/T body ("except it's 1/4") is part
    // of the copy's COPIABLE values, so the live copy is 1/4 and NOT the 5/5 it
    // copied. Production-path discriminator for the `parse_subject_pt_only`
    // arm: with that arm unregistered, the body is skipped fail-soft and the
    // copy resolves as a plain copy at the copied creature's P/T.
    //
    // REACH GUARD: the name assertion directly above proves the copy effect is
    // live (the object is "Facetaker Target", not "Cephalid Facetaker"), so a
    // 1/4 read here cannot be the printed body of an uncopied Facetaker; and
    // the copy source is a 5/5, so 1/4 can only come from the exception.
    assert_eq!(
        (
            runner.state().objects[&target].power,
            runner.state().objects[&target].toughness
        ),
        (Some(5), Some(5)),
        "reach guard: the copied creature must really be a 5/5"
    );
    assert_eq!(
        (
            runner.state().objects[&facetaker].power,
            runner.state().objects[&facetaker].toughness
        ),
        (Some(1), Some(4)),
        "the live copy must take the exception's overridden 1/4, not the copied 5/5"
    );

    execute_cleanup(runner.state_mut(), &mut Vec::new());
    flush_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&facetaker].name,
        "Cephalid Facetaker",
        "the printed end-of-turn copy duration must expire during cleanup"
    );
}

#[test]
fn cephalid_facetaker_live_copy_does_not_follow_it_to_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P0, "Facetaker Target", 5, 5).id();
    let facetaker = scenario
        .add_creature_from_oracle(P0, "Cephalid Facetaker", 1, 4, FACETAKER_ORACLE)
        .id();
    let mut runner = scenario.build();

    resolve_facetaker_combat_trigger(&mut runner, target);
    assert_eq!(
        runner.state().objects[&facetaker].name,
        "Facetaker Target",
        "precondition: the until-end-of-turn copy must be live before it changes zones"
    );

    assert!(
        !move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::effect(facetaker, Zone::Hand, target),
            &mut Vec::new(),
        ),
        "Facetaker's return to hand must complete without a replacement choice"
    );
    flush_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&facetaker].zone, Zone::Hand);
    assert_eq!(
        runner.state().objects[&facetaker].name,
        "Cephalid Facetaker",
        "a new hand incarnation must retain its own identity, not the live battlefield copy"
    );
}

#[test]
fn glasspool_mimic_copy_target_choice_does_not_survive_a_return_to_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 205.1b reach guard: give the copied creature its own subtype
    // (Merfolk) so retention of the copy source's subtypes is provable
    // rather than vacuous.
    let target = scenario
        .add_creature(P0, "Glasspool Target", 5, 5)
        .with_subtypes(vec!["Merfolk"])
        .id();
    let mimic = scenario
        .add_creature_to_hand_from_oracle(P0, "Glasspool Mimic", 0, 0, GLASSPOOL_ORACLE)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&mimic].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: mimic,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Glasspool Mimic");
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Glasspool Mimic's enter-as-copy replacement");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CopyTargetChoice { .. }
    ));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("choose Glasspool Mimic's copy target");

    assert_eq!(runner.state().objects[&mimic].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&mimic].name, "Glasspool Target");

    // CR 707.9b: the copy exception's "except it's a Shapeshifter Rogue in
    // addition to its other types" body is part of the copy's COPIABLE
    // values, so the live copy carries Shapeshifter AND Rogue as two
    // separate subtypes, never a fabricated single "Shapeshifter Rogue"
    // value. CR 205.1b: the "in addition to its other types" carve-out
    // means the copy RETAINS the copied creature's own subtypes (here,
    // Merfolk) rather than replacing them.
    //
    // Production-path discriminator for `parse_its_a_type_in_addition`
    // (`append_color_and_type_modifications`'s per-word classification):
    // with that arm's routing disabled, the multi-word descriptor list
    // falls through unclassified and the live copy's subtypes are just the
    // copied Merfolk, missing both Shapeshifter and Rogue.
    let live_subtypes = &runner.state().objects[&mimic].card_types.subtypes;
    assert!(
        live_subtypes.contains(&"Shapeshifter".to_string()),
        "expected Shapeshifter as its own subtype, got {live_subtypes:?}"
    );
    assert!(
        live_subtypes.contains(&"Rogue".to_string()),
        "expected Rogue as its own subtype, got {live_subtypes:?}"
    );
    assert!(
        !live_subtypes.contains(&"Shapeshifter Rogue".to_string()),
        "must not fabricate a single \"Shapeshifter Rogue\" subtype, got {live_subtypes:?}"
    );
    assert!(
        live_subtypes.contains(&"Merfolk".to_string()),
        "CR 205.1b: the copy must retain the copied creature's own subtypes, got {live_subtypes:?}"
    );

    assert!(
        !move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::effect(mimic, Zone::Hand, target),
            &mut Vec::new(),
        ),
        "Glasspool Mimic's return to hand must complete without a replacement choice"
    );
    flush_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&mimic].name,
        "Glasspool Mimic",
        "the permanent copy's recipient pin must not apply to the new hand incarnation"
    );
}

//! My Undead Horde Awakens: the dies trigger only fires for creatures put
//! onto the battlefield with the scheme.
//!
//! Oracle: "(An ongoing scheme remains face up until it's abandoned.)
//! At the beginning of your end step, you may put target creature card from an opponent's graveyard onto the battlefield under your control.
//! When a creature put onto the battlefield with this scheme dies, abandon this scheme."
//!
//! CR 603.6a + CR 400.7: the scheme's reanimation stamps the entrant's
//! `entered_via_ability_source`; the dies trigger's `PlacedByAbilitySource`
//! condition reads that stamp from the exiting record's pre-move snapshot
//! (CR 608.2h — the live stamp is cleared on exit). CR 701.33a: only a
//! face-up ONGOING scheme may be abandoned — reaching the face-down
//! bottom-of-deck end state proves the trigger fired.
//!
//! Revert-to-red: drop the trigger condition back to `None` — the negative
//! phase fails (any death abandons). Lower the head back to `Unknown` — the
//! positive phase fails (nothing abandons).

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MY_UNDEAD_HORDE_AWAKENS: &str = "(An ongoing scheme remains face up until it's abandoned.)\nAt the beginning of your end step, you may put target creature card from an opponent's graveyard onto the battlefield under your control.\nWhen a creature put onto the battlefield with this scheme dies, abandon this scheme.";

fn scheme_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Scheme".to_string()], &[]);
    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "{name}: document is gapped"
    );
    let mut face = CardFace {
        name: name.to_string(),
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Scheme);
    face.card_type.supertypes.push(Supertype::Ongoing);
    synthesize_archenemy(&mut face);
    // The dies head binds `PlacedByAbilitySource`, so the parsed face
    // carries no gap.
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(gaps.is_empty(), "{name}: gaps: {gaps:?}");
    face
}

fn red_mana() -> ManaUnit {
    ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])
}

fn scheme_object_id(runner: &GameRunner, name: &str) -> ObjectId {
    *runner
        .state()
        .command_zone
        .iter()
        .find(|id| runner.state().objects[*id].name == name)
        .expect("scheme must be face up in the command zone")
}

/// Drive the end-step reanimation trigger: assert P0 is offered the two
/// graveyard creatures as targets, choose the bears, accept the "you may",
/// and resolve to an empty stack at priority.
fn reanimate_bears(runner: &mut GameRunner, bears: ObjectId, elk: ObjectId) {
    let mut saw_target = false;
    let mut saw_optional = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection {
                player,
                target_slots,
                selection,
                ..
            } => {
                assert_eq!(player, P0, "the archenemy targets the reanimation");
                let slot = &target_slots[selection.current_slot];
                assert!(
                    slot.legal_targets.contains(&TargetRef::Object(bears)),
                    "bears must be a legal target, got {:?}",
                    slot.legal_targets
                );
                assert!(
                    slot.legal_targets.contains(&TargetRef::Object(elk)),
                    "the second creature keeps targeting non-trivial, got {:?}",
                    slot.legal_targets
                );
                saw_target = true;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(bears)),
                    })
                    .expect("choosing the bears must be accepted");
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(player, P0, "the archenemy decides the 'you may'");
                saw_optional = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting the reanimation must be accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while reanimating: {other:?}"),
        }
    }
    assert!(saw_target, "the reanimation target prompt must be offered");
    assert!(saw_optional, "the 'you may' decision must be offered");
    assert!(
        runner.state().stack.is_empty(),
        "the reanimation trigger must have resolved"
    );
}

/// Bolt a creature through the production cast pipeline, topping up P0's
/// pool first (pools empty across phases, so scenario seeding cannot reach
/// the end step).
fn bolt_creature(runner: &mut GameRunner, bolt: ObjectId, victim: ObjectId) {
    runner.state_mut().add_mana_to_pool(P0, red_mana());
    let outcome = runner.cast(bolt).target_objects(&[victim]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "bolt resolution must end at priority, got {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        outcome.state().objects[&victim].zone,
        Zone::Graveyard,
        "bolt victim must have died"
    );
}

#[test]
fn only_scheme_reanimated_creature_dying_abandons_horde() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    // Two reanimation candidates in P1's graveyard so target selection is
    // non-trivial (a single legal target would auto-select with no prompt).
    let bears = scenario
        .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
        .id();
    let elk = scenario
        .add_creature_to_graveyard(P1, "Golden Elk", 2, 2)
        .id();
    // The negative control: P0's own creature, never stamped by the scheme.
    let lion = scenario.add_creature(P0, "Silvercoat Lion", 2, 2).id();
    let bolt_a = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Lightning Bolt",
            true,
            "Lightning Bolt deals 3 damage to any target.",
        )
        .id();
    let bolt_b = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Lightning Bolt",
            true,
            "Lightning Bolt deals 3 damage to any target.",
        )
        .id();
    let mut runner = scenario.build();

    // Production scheme-deck installation, then designate the archenemy.
    let face = scheme_face("My Undead Horde Awakens", MY_UNDEAD_HORDE_AWAKENS);
    create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    runner.advance_to_phase(Phase::PreCombatMain);
    let scheme = scheme_object_id(&runner, "My Undead Horde Awakens");

    // CR 508.1: attack with no one, then reach the end step.
    runner.advance_to_combat();
    runner.declare_attackers(&[]).expect("declare no attackers");

    // CR 513.2: the end-step trigger fires; reanimate the bears.
    runner.advance_to_end_step();
    reanimate_bears(&mut runner, bears, elk);
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Battlefield,
        "the bears must be reanimated"
    );
    assert_eq!(
        runner.state().objects[&bears].controller,
        P0,
        "the bears enter under the archenemy's control"
    );
    // CR 603.6a reach-guard: the abandon trigger reads this stamp.
    assert_eq!(
        runner.state().objects[&bears].entered_via_ability_source,
        Some(scheme),
        "reanimation must stamp the entrant with the scheme id"
    );

    // NEGATIVE: an unstamped creature dying does not abandon the scheme.
    bolt_creature(&mut runner, bolt_a, lion);
    assert!(
        !runner.state().objects[&scheme].face_down,
        "an ordinary death must not abandon the scheme"
    );
    assert!(
        runner.state().stack.is_empty(),
        "no abandon trigger may be pending after an ordinary death"
    );

    // POSITIVE: the reanimated bears dying abandons the scheme.
    bolt_creature(&mut runner, bolt_b, bears);
    for _ in 0..32 {
        if runner.state().objects[&scheme].face_down {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                assert!(
                    !runner.state().stack.is_empty(),
                    "the abandon trigger must be on the stack"
                );
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while abandoning: {other:?}"),
        }
    }
    assert!(
        runner.state().objects[&scheme].face_down,
        "the reanimated bears dying must abandon the scheme"
    );
    assert_eq!(
        runner.state().scheme_deck.back(),
        Some(&scheme),
        "abandoned scheme must be on the bottom of the scheme deck"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the abandon trigger must have resolved"
    );
}

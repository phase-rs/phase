//! Imprison This Insolent Wretch: "the chosen player" trigger subjects.
//!
//! Oracle: "(An ongoing scheme remains face up until it's abandoned.)
//! When you set this scheme in motion, choose an opponent.
//! Permanents the chosen player controls don't untap during their untap step.
//! When the chosen player is attacked or becomes the target of a spell or ability, abandon this scheme."
//!
//! CR 607.2d: the set-in-motion choice persists as the scheme's linked choice;
//! both abandon triggers read it via `SourceChosenPlayer`. CR 701.33a: only a
//! face-up ONGOING scheme may be abandoned — Imprison is ongoing, so reaching
//! the face-down bottom-of-deck end state proves the trigger fired (the CR
//! 904.10 SBA only touches non-ongoing schemes).
//!
//! Revert-to-red: lower either trigger back to `Unknown` — no trigger fires
//! and Imprison stays face up in the command zone.

use engine::database::synthesis::synthesize_archenemy;
use engine::game::combat::AttackTarget;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::ability::{ChoiceType, ChosenAttribute};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const IMPRISON_THIS_INSOLENT_WRETCH: &str = "(An ongoing scheme remains face up until it's abandoned.)\nWhen you set this scheme in motion, choose an opponent.\nPermanents the chosen player controls don't untap during their untap step.\nWhen the chosen player is attacked or becomes the target of a spell or ability, abandon this scheme.";

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
    // Both abandon triggers bind `SourceChosenPlayer`, so the parsed face
    // carries no gap.
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(gaps.is_empty(), "{name}: gaps: {gaps:?}");
    face
}

fn red_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect()
}

/// Install Imprison as P0's scheme deck, set it in motion, and choose P1.
/// Returns the face-up scheme object id with the choice persisted.
fn scheme_in_motion_with_p1_chosen(runner: &mut GameRunner) -> ObjectId {
    let face = scheme_face(
        "Imprison This Insolent Wretch",
        IMPRISON_THIS_INSOLENT_WRETCH,
    );
    create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(
        runner.state().scheme_deck.is_empty(),
        "Imprison must have left the scheme deck"
    );

    // Resolve the "choose an opponent" set-in-motion trigger and choose P1.
    let mut saw_choice = false;
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority into the choice");
            }
            WaitingFor::NamedChoice {
                player,
                choice_type,
                options,
                ..
            } => {
                assert_eq!(player, P0, "the archenemy makes the choice");
                assert!(
                    matches!(choice_type, ChoiceType::Opponent { .. }),
                    "expected an opponent choice, got {choice_type:?}"
                );
                assert_eq!(options, &["1"], "only P1 may be offered, got {options:?}");
                saw_choice = true;
                runner
                    .act(GameAction::ChooseOption {
                        choice: "1".to_string(),
                    })
                    .expect("choosing P1 must be accepted");
            }
            other => panic!("unexpected prompt while choosing: {other:?}"),
        }
    }
    assert!(saw_choice, "the set-in-motion choice must be offered");

    let scheme = *runner
        .state()
        .command_zone
        .iter()
        .find(|id| runner.state().objects[*id].name == "Imprison This Insolent Wretch")
        .expect("Imprison must be face up in the command zone");
    // CR 607.2d reach-guard: both abandon triggers read this persisted choice.
    assert!(
        runner.state().objects[&scheme]
            .chosen_attributes
            .contains(&ChosenAttribute::Player(P1)),
        "Imprison must store P1 as its chosen player"
    );
    scheme
}

fn assert_abandoned(runner: &GameRunner, scheme: ObjectId) {
    let obj = &runner.state().objects[&scheme];
    assert!(obj.face_down, "abandoned scheme must be face down");
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

#[test]
fn chosen_player_attacked_abandons_imprison() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    let scheme = scheme_in_motion_with_p1_chosen(&mut runner);

    // CR 508.1a: P0 attacks the chosen player P1 with the Bears.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bears, AttackTarget::Player(P1))])
        .expect("declare attackers");

    // Drive priority until the abandon trigger resolves. Attacking a
    // planeswalker P1 controls or declaring no attack must NOT abandon —
    // that negative is pinned by the attack_target_filter parser test; here
    // the face-down end state proves the trigger fired.
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
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("declare no blockers");
            }
            other => panic!("unexpected prompt while abandoning: {other:?}"),
        }
    }
    assert_abandoned(&runner, scheme);
}

#[test]
fn chosen_player_targeted_abandons_imprison() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    scenario.with_mana_pool(P0, red_mana(1));
    let bolt = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Lightning Bolt",
            true,
            "Lightning Bolt deals 3 damage to any target.",
        )
        .id();
    let mut runner = scenario.build();

    let scheme = scheme_in_motion_with_p1_chosen(&mut runner);

    // CR 603.2e + CR 115.1: targeting the chosen player P1 with Bolt fires
    // the abandon trigger above the spell.
    let outcome = runner.cast(bolt).target_player(P1).resolve();
    outcome.assert_life_delta(P1, -3);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution must end at priority, got {:?}",
        outcome.final_waiting_for()
    );
    assert_abandoned(&runner, scheme);
}

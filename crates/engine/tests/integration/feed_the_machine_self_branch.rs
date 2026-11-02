//! Feed the Machine, self branch: the announced opponent sacrifices their own
//! creatures — never the archenemy's.
//!
//! Oracle: "When you set this scheme in motion, target opponent chooses self or
//! others. If that player chooses self, the player sacrifices two creatures of
//! their choice. If the player chooses others, each of your other opponents
//! sacrifices a creature of their choice."
//!
//! CR 904.9 sets the scheme in motion as the archenemy's precombat main begins;
//! CR 115.1 declares the opponent target when the trigger goes on the stack;
//! CR 701.21a restricts the sacrifice to permanents the sacrificing player
//! controls. The self-branch subject ("the player") is a `ParentTarget`
//! anaphor for the announced target; the Sacrifice injection stamps it as
//! `TargetPlayer`, and `resolve_sacrifice_scope` reads the announced opponent
//! from the sub-ability's propagated targets at resolution time.
//!
//! Revert-to-red: drop the `ParentTarget` arm from
//! `player_filter_as_controller_ref` — the controller scope is never stamped,
//! the sacrifice falls back to the archenemy, and the prompt-player /
//! graveyard assertions fail.

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::ability::{ChoiceType, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FEED_THE_MACHINE: &str = "When you set this scheme in motion, target opponent chooses self or others. If that player chooses self, the player sacrifices two creatures of their choice. If the player chooses others, each of your other opponents sacrifices a creature of their choice.";

fn scheme_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Scheme".to_string()], &[]);
    assert_eq!(
        parsed.triggers.len(),
        1,
        "{name} must parse to exactly one trigger"
    );
    assert!(
        !format!("{:?}", parsed.triggers).contains("Unimplemented"),
        "{name}: trigger body is gapped"
    );
    let mut face = CardFace {
        name: name.to_string(),
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Scheme);
    synthesize_archenemy(&mut face);
    face
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects.get(&id).expect("object exists").zone
}

#[test]
fn feed_the_machine_self_branch_sacrifices_only_the_choosers_creatures() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    let p0c1 = scenario.add_vanilla(P0, 2, 2);
    let p0c2 = scenario.add_vanilla(P0, 3, 3);
    // Three creatures for the victim: the pool (3) exceeds the count (2), so
    // the sacrifice surfaces its interactive prompt instead of fast-pathing.
    let p1c1 = scenario.add_vanilla(P1, 2, 2);
    let p1c2 = scenario.add_vanilla(P1, 3, 3);
    let p1c3 = scenario.add_vanilla(P1, 4, 4);
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    let mut runner = scenario.build();

    // Production scheme-deck installation, then designate the archenemy.
    let face = scheme_face("Feed the Machine", FEED_THE_MACHINE);
    create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    // CR 904.9: entering the archenemy's precombat main sets the scheme in
    // motion; CR 115.1: the trigger announces its opponent target.
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(
        runner.state().scheme_deck.is_empty(),
        "the scheme must have left the scheme deck"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the set-in-motion trigger must be on the stack"
    );
    let announced: Vec<TargetRef> = runner.state().stack[0]
        .ability()
        .expect("trigger has ability")
        .targets
        .clone();
    assert_eq!(
        announced,
        vec![TargetRef::Player(P1)],
        "the trigger must announce P1 as its opponent target"
    );

    let mut saw_choice = false;
    let mut saw_sacrifice = false;
    for _ in 0..128 {
        let wf = runner.state().waiting_for.clone();
        match wf {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                // Two-player games auto-bind the single legal opponent, but
                // answer explicitly if the prompt is ever offered.
                let slot = &target_slots[selection.current_slot];
                assert!(
                    slot.legal_targets.contains(&TargetRef::Player(P1)),
                    "P1 must be a legal opponent target, got {:?}",
                    slot.legal_targets
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("choosing P1 must be accepted");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::NamedChoice {
                player,
                choice_type,
                options,
                ..
            } => {
                assert_eq!(player, P1, "the targeted opponent must choose");
                assert!(
                    matches!(choice_type, ChoiceType::Labeled { .. }),
                    "expected the self-or-others label choice, got {choice_type:?}"
                );
                assert!(
                    options.iter().any(|o| o == "Self"),
                    "options must include Self, got {options:?}"
                );
                runner
                    .act(GameAction::ChooseOption {
                        choice: "Self".to_string(),
                    })
                    .expect("choosing Self must be accepted");
                saw_choice = true;
            }
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                ..
            } => {
                // CR 701.21a: only the choosing opponent sacrifices, choosing
                // among the creatures THEY control — never the archenemy's.
                assert_eq!(
                    player, P1,
                    "the sacrifice prompt must go to the choosing opponent"
                );
                assert_eq!(count, 2, "Feed the Machine sacrifices two creatures");
                assert!(
                    cards.contains(&p1c1) && cards.contains(&p1c2) && cards.contains(&p1c3),
                    "all three of P1's creatures must be eligible, got {cards:?}"
                );
                assert!(
                    !cards.contains(&p0c1) && !cards.contains(&p0c2),
                    "the archenemy's creatures must not be eligible, got {cards:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![p1c1, p1c2],
                    })
                    .expect("sacrificing two creatures must be accepted");
                saw_sacrifice = true;
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected prompt while resolving Feed the Machine: {other:?}"),
        }
    }

    assert!(
        saw_choice,
        "the targeted opponent must be offered self-or-others"
    );
    assert!(saw_sacrifice, "the self branch must prompt a sacrifice");
    assert!(
        runner.state().stack.is_empty(),
        "the scheme trigger must fully resolve"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must end at priority, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(zone_of(&runner, p1c1), Zone::Graveyard);
    assert_eq!(zone_of(&runner, p1c2), Zone::Graveyard);
    assert_eq!(
        zone_of(&runner, p1c3),
        Zone::Battlefield,
        "the unchosen creature survives"
    );
    assert_eq!(
        zone_of(&runner, p0c1),
        Zone::Battlefield,
        "the archenemy sacrifices nothing on the self branch"
    );
    assert_eq!(
        zone_of(&runner, p0c2),
        Zone::Battlefield,
        "the archenemy sacrifices nothing on the self branch"
    );
}

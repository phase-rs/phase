//! My Forces Are Innumerable: the opponents' collective sacrifice stays
//! honestly unsupported — no invented player target, no fail-open archenemy
//! sacrifice.
//!
//! Oracle (verbatim): "(An ongoing scheme remains face up until it's
//! abandoned.) At the beginning of your end step, create a 3/3 black Horror
//! creature token. At the beginning of your opponents' end step, they may
//! sacrifice two creatures of their choice. If they do, abandon this scheme."
//!
//! CR 904.11 keeps the scheme face up (ongoing); the opponents'-end-step
//! trigger names no target, and "they" is the collective opponents — not a
//! singular announced player. There is no team-cost class yet, so the
//! sacrifice clause must export an explicit `unbound_subject` gap: promoting
//! the bare anaphor to `TargetPlayer` would invent a player target slot the
//! card does not have (the archenemy would pick one opponent to edict), and
//! leaving it unscoped would fail open to the archenemy sacrificing its own
//! creatures.
//!
//! Revert-to-red: restore the unconditional `ParentTarget => TargetPlayer`
//! promotion — the parse test finds the invented `valid_target` +
//! `TargetPlayer` refs, and the runtime test observes a target prompt plus a
//! sacrifice.
//!
//! The runtime leg runs a four-player archenemy game through the shared
//! opponents' end step: the trigger must fire (telemetry records the gap),
//! offer no target or sacrifice prompt, sacrifice nothing, and leave the
//! scheme face up (the "If they do" abandon stays conditional on a sacrifice
//! that never happens).

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::ability::{AbilityCondition, Effect, EffectOutcomeSignal};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

const MY_FORCES: &str = "(An ongoing scheme remains face up until it's abandoned.)\nAt the beginning of your end step, create a 3/3 black Horror creature token.\nAt the beginning of your opponents' end step, they may sacrifice two creatures of their choice. If they do, abandon this scheme.";

fn scheme_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Scheme".to_string()], &[]);
    let mut face = CardFace {
        name: name.to_string(),
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Scheme);
    // Real type line "Ongoing Scheme": without the supertype the 904.10 SBA
    // turns the scheme face down and returns it to the deck.
    face.card_type.supertypes.push(Supertype::Ongoing);
    synthesize_archenemy(&mut face);
    face
}

#[test]
fn my_forces_collective_sacrifice_exports_honest_gap_without_target() {
    let parsed = parse_oracle_text(
        MY_FORCES,
        "My Forces Are Innumerable",
        &[],
        &["Scheme".to_string()],
        &[],
    );
    assert_eq!(
        parsed.triggers.len(),
        2,
        "both end-step triggers must parse, got {}",
        parsed.triggers.len()
    );
    // Reach-guard: the Horror-token trigger is fully supported, so a
    // whole-card parse failure cannot masquerade as the honest gap.
    assert!(
        format!("{:?}", parsed.triggers[0]).contains("Token"),
        "your-end-step trigger must create the Horror token, got {:?}",
        parsed.triggers[0]
    );
    let opponents = &parsed.triggers[1];
    assert_eq!(
        opponents.valid_target, None,
        "the opponents'-end-step trigger must declare no player target"
    );
    assert!(
        matches!(
            &*opponents.execute.as_ref().expect("trigger has a body").effect,
            Effect::Unimplemented { name, .. } if name == "unbound_subject"
        ),
        "the collective sacrifice must gap explicitly, got {:?}",
        opponents.execute.as_ref().map(|d| &d.effect)
    );
    assert!(
        !format!("{parsed:?}").contains("TargetPlayer"),
        "no TargetPlayer ref may survive anywhere on the card"
    );
    // The abandon continuation stays conditional on a sacrifice occurring.
    let abandon = opponents
        .execute
        .as_ref()
        .and_then(|d| d.sub_ability.as_deref())
        .expect("abandon continuation survives the gap");
    assert!(
        matches!(&*abandon.effect, Effect::AbandonScheme),
        "expected the AbandonScheme continuation, got {:?}",
        abandon.effect
    );
    assert_eq!(
        abandon.condition,
        Some(AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        }),
        "abandon must stay conditional on the sacrifice happening"
    );
}

fn face_up_scheme_id(runner: &engine::game::scenario::GameRunner) -> Option<ObjectId> {
    runner
        .state()
        .command_zone
        .iter()
        .find(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.name == "My Forces Are Innumerable" && !o.face_down)
        })
        .copied()
}

#[test]
fn my_forces_shared_end_step_offers_no_target_and_sacrifices_nothing() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 4, 42);
    scenario.at_phase(Phase::Draw);
    let mut victims = Vec::new();
    for player in [P1, P2, P3] {
        victims.push(scenario.add_vanilla(player, 2, 2));
        victims.push(scenario.add_vanilla(player, 3, 3));
    }

    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Library {i}"));
        scenario.add_card_to_library_top(P3, &format!("P3 Library {i}"));
    }
    let mut runner = scenario.build();
    let face = scheme_face("My Forces Are Innumerable", MY_FORCES);
    create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    // CR 904.9: precombat main sets the ongoing scheme in motion; as an
    // ongoing scheme it stays face up instead of turning face down.
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(
        runner.state().scheme_deck.is_empty(),
        "the scheme must have left the scheme deck (stack={}, phase={:?}, waiting={:?})",
        runner.state().stack.len(),
        runner.state().phase,
        runner.state().waiting_for
    );
    // No "when set in motion" trigger exists, so the stack stays empty; the
    // ongoing scheme simply remains face up (CR 904.11).
    assert!(
        runner.state().stack.is_empty(),
        "My Forces has no set-in-motion trigger: the stack must be empty"
    );
    assert!(
        face_up_scheme_id(&runner).is_some(),
        "My Forces must be face up in the command zone after set-in-motion"
    );

    // Drive through P0's end step (Horror token) into P1's end step (the
    // opponents'-end-step trigger). Any target prompt or sacrifice prompt
    // is the invented-target regression: fail loudly.
    let mut saw_horror = false;
    let mut saw_opponents_trigger = false;
    for _ in 0..512 {
        if runner.state().active_player == PlayerId(1)
            && runner.state().phase == Phase::End
            && saw_opponents_trigger
            && runner.state().stack.is_empty()
        {
            break;
        }
        // Keep priority moving; stops at any prompt. Only advance while
        // some phase boundary is still ahead: re-running advance (and its
        // trigger scan) while parked at End re-collects the end-step
        // trigger on every iteration.
        if runner.state().phase != Phase::End {
            runner.advance_to_phase(Phase::End);
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                // Four seats rotate priority; pass as whoever holds it.
                let _ =
                    engine::game::apply_as_current(runner.state_mut(), GameAction::PassPriority);
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DeclareAttackers { .. } => {
                let _ = engine::game::apply_as_current(
                    runner.state_mut(),
                    GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    },
                );
            }
            WaitingFor::DeclareBlockers { .. } => {
                let _ = engine::game::apply_as_current(
                    runner.state_mut(),
                    GameAction::DeclareBlockers {
                        assignments: vec![],
                    },
                );
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                panic!(
                    "no target prompt may appear: the sacrifice names no target (state: {:?})",
                    runner.state().waiting_for
                );
            }
            // EffectZoneChoice (sacrifice prompt), NamedChoice (may
            // prompt), or anything else interactive is equally wrong:
            // the gapped clause offers no choices.
            other => panic!("no choice prompt may appear, got {other:?}"),
        }
        if runner
            .state()
            .objects
            .values()
            .any(|o| o.name.contains("Horror") && o.zone == Zone::Battlefield)
        {
            saw_horror = true;
        }
        if runner.state().stack.iter().any(|entry| {
            entry
                .ability()
                .map(|a| format!("{:?}", a.effect).contains("unbound_subject"))
                .unwrap_or(false)
        }) {
            saw_opponents_trigger = true;
        }
        // Telemetry records the executed gap: the trigger FIRED (reach-guard
        // against a vacuous pass where the end step never triggers).
        if runner
            .state()
            .unimplemented_oracle_ids
            .iter()
            .any(|id| id.contains("My Forces"))
        {
            saw_opponents_trigger = true;
        }
    }

    assert!(
        saw_horror,
        "reach-guard: the Horror token proves P0's end step fired"
    );
    assert!(
        saw_opponents_trigger,
        "reach-guard: the opponents'-end-step trigger must have fired and executed its gapped body"
    );
    for id in &victims {
        assert_eq!(
            runner.state().objects.get(id).map(|o| o.zone),
            Some(Zone::Battlefield),
            "no opponent creature may be sacrificed"
        );
    }
    assert!(
        face_up_scheme_id(&runner).is_some(),
        "the scheme must still be face up: abandon requires a sacrifice that never happened"
    );
}

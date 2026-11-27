//! My Laughter Echoes: the watching trigger fires for other non-ongoing
//! schemes, ignores ongoing schemes (including itself), and re-fires the
//! watched scheme when its controller pays the abandon cost.
//!
//! Oracle: "(An ongoing scheme remains face up until it's abandoned.)
//! Whenever you set a non-ongoing scheme in motion, you may abandon this scheme. If you do, set that scheme in motion again."
//!
//! CR 904.9 + CR 603.2: "whenever you set a non-ongoing scheme in motion"
//! watches ANOTHER scheme — the same SetInMotion mode as the self trigger,
//! scoped by valid_card (Typed Scheme + NotSupertype Ongoing) instead of
//! source identity. CR 118.12: "you may abandon this scheme. If you do, set
//! that scheme in motion again" is an optional instruction plus an
//! effect-performed followup re-firing the triggering event's scheme
//! (CR 701.32b + CR 904.10).
//!
//! Revert-to-red: drop NotSupertype from the watch filter — the turn-1
//! negative fails (Laughter fires for its own ongoing self). Drop the
//! watching branch from `match_set_in_motion` — the turn-2 positive fails
//! (five tokens, not ten). Break `set_in_motion_again` — same five-token
//! failure with Laughter still abandoned.

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MY_LAUGHTER_ECHOES: &str = "(An ongoing scheme remains face up until it's abandoned.)\nWhenever you set a non-ongoing scheme in motion, you may abandon this scheme. If you do, set that scheme in motion again.";
const ROOTS_OF_ALL_EVIL: &str =
    "When you set this scheme in motion, create five 1/1 green Saproling creature tokens.";

fn scheme_face(name: &str, oracle: &str, ongoing: bool) -> CardFace {
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
    if ongoing {
        face.card_type.supertypes.push(Supertype::Ongoing);
    }
    synthesize_archenemy(&mut face);
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(gaps.is_empty(), "{name}: gaps: {gaps:?}");
    face
}

fn scheme_object_id(runner: &GameRunner, name: &str) -> ObjectId {
    *runner
        .state()
        .command_zone
        .iter()
        .find(|id| runner.state().objects[*id].name == name)
        .expect("scheme must be face up in the command zone")
}

fn saprolings_controlled_by(runner: &GameRunner, player: engine::types::player::PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == Zone::Battlefield
                && obj.controller == player
                && obj.name.contains("Saproling")
        })
        .count()
}

fn battlefield_names(runner: &GameRunner) -> Vec<String> {
    let mut names: Vec<String> = runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.zone == Zone::Battlefield)
        .map(|obj| obj.name.clone())
        .collect();
    names.sort();
    names
}

#[test]
fn laughter_echoes_ignores_ongoing_and_refires_nonongoing() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    // A creature for P0 so DeclareAttackers stops each combat (with no
    // creatures the prompt is skipped and phase advances run away through
    // whole turns). It never attacks; the Saproling count below is by name.
    scenario.add_creature(P0, "Silvercoat Lion", 2, 2);
    let mut runner = scenario.build();

    // Production scheme-deck installation: Laughter on top, Roots beneath.
    // Both faces assert gap-free inside `scheme_face`.
    let laughter_face = scheme_face("My Laughter Echoes", MY_LAUGHTER_ECHOES, true);
    let roots_face = scheme_face("Roots of All Evil", ROOTS_OF_ALL_EVIL, false);
    create_scheme_deck_card(runner.state_mut(), &laughter_face, P0);
    create_scheme_deck_card(runner.state_mut(), &roots_face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    // Turn 1: Laughter is set in motion. NEGATIVE: its watching trigger must
    // not fire for its own ongoing self — the main phase must drain to
    // combat with no prompt and an empty stack.
    runner.advance_to_phase(Phase::PreCombatMain);
    let laughter = scheme_object_id(&runner, "My Laughter Echoes");
    runner.advance_to_combat();
    assert_eq!(
        runner.state().phase,
        Phase::DeclareAttackers,
        "no trigger prompt may interrupt turn-1 main; waiting_for={:?} stack_len={} turn={} active={:?}",
        runner.state().waiting_for,
        runner.state().stack.len(),
        runner.state().turn_number,
        runner.state().active_player
    );
    assert!(
        runner.state().stack.is_empty(),
        "Laughter must not trigger for its own ongoing set-in-motion"
    );
    assert!(
        !runner.state().objects[&laughter].face_down,
        "Laughter must still be face up (the watcher exists and could have fired)"
    );
    runner.declare_attackers(&[]).expect("declare no attackers");

    // Drive to P0's second precombat main: Roots is set in motion there.
    for _ in 0..8 {
        runner.advance_to_phase(Phase::PreCombatMain);
        if runner
            .state()
            .command_zone
            .iter()
            .any(|id| runner.state().objects[id].name == "Roots of All Evil")
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            // P1's precombat main: no scheme moves (P1 is not the archenemy).
            // Pass out of it — `advance_to_phase` no-ops when already at its
            // target, so looping without passing would spin here.
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while driving to turn 2: {other:?}"),
        }
    }
    let roots = scheme_object_id(&runner, "Roots of All Evil");

    // POSITIVE: Roots' own trigger plus Laughter's watching trigger resolve;
    // accepting the "you may" abandons Laughter and re-fires Roots.
    let mut saw_may = false;
    for _ in 0..64 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(player, P0, "the archenemy decides the 'you may'");
                saw_may = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting the abandon must be accepted");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while re-firing: {other:?}"),
        }
    }
    assert!(saw_may, "the 'you may abandon' decision must be offered");
    assert!(
        runner.state().stack.is_empty(),
        "all scheme triggers must have resolved"
    );
    assert_eq!(
        saprolings_controlled_by(&runner, P0),
        10,
        "Roots must have fired twice (5 + 5 Saprolings); battlefield: {:?}",
        battlefield_names(&runner)
    );
    assert!(
        runner.state().objects[&laughter].face_down,
        "accepting the 'you may' must abandon Laughter"
    );
    assert!(
        runner.state().objects[&roots].face_down,
        "CR 904.10: the non-ongoing scheme turns face down once its triggers resolve"
    );
    assert_eq!(
        runner.state().scheme_deck.back(),
        Some(&roots),
        "CR 904.10: Roots reaches the bottom of the scheme deck after Laughter's abandon"
    );
    assert!(
        runner.state().scheme_deck.contains(&laughter),
        "abandoned Laughter must be on the bottom of the scheme deck"
    );
}

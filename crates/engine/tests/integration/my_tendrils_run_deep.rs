//! My Tendrils Run Deep: the end-step trigger draws two cards and then
//! abandons the scheme when its controller controls six or more lands.
//!
//! Oracle: "(An ongoing scheme remains face up until it's abandoned.)
//! You may play an additional land on each of your turns.
//! At the beginning of your end step, if you control six or more lands, draw two cards, then abandon this scheme."
//!
//! CR 603.4: "if you control six or more lands" is an intervening-if —
//! the trigger never fires below the threshold. CR 608.2c: ", then abandon
//! this scheme" is a chained follow-up clause, not part of the draw.
//!
//! Revert-to-red: drop "abandon" from the clause-starter verb table — the
//! draw head refuses the whole body (positive fails: no draw, no abandon).
//! Break the abandon resolver — the scheme stays face up. Break the land
//! count — the threshold flips the wrong way.

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
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const MY_TENDRILS_RUN_DEEP: &str = "(An ongoing scheme remains face up until it's abandoned.)\nYou may play an additional land on each of your turns.\nAt the beginning of your end step, if you control six or more lands, draw two cards, then abandon this scheme.";

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
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(gaps.is_empty(), "{name}: gaps: {gaps:?}");
    face
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("the player is seated")
        .hand
        .len()
}

fn lands_on_battlefield(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == engine::types::zones::Zone::Battlefield
                && obj.controller == player
                && obj.card_types.core_types.contains(&CoreType::Land)
        })
        .count()
}

/// Resolve every pending trigger with no choices (the draw/abandon chain is
/// prompt-free).
fn drain_triggers(runner: &mut GameRunner) {
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
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while draining triggers: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
}

/// Drive to `target`, declaring no attackers/blockers through combat and
/// passing priority. Panics on any other prompt.
fn drive_to_phase(runner: &mut GameRunner, target: Phase) {
    for _ in 0..32 {
        runner.advance_to_phase(target);
        if runner.state().phase == target {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("declare no blockers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected prompt driving to {target:?}: {other:?}"),
        }
    }
    panic!("never reached {target:?}");
}

fn scheme_id(runner: &GameRunner, name: &str) -> ObjectId {
    *runner
        .state()
        .command_zone
        .iter()
        .find(|id| runner.state().objects[*id].name == name)
        .expect("scheme must be face up in the command zone")
}

fn setup_tendrils(land_count: usize) -> (GameRunner, ObjectId) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    for _ in 0..land_count {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let mut runner = scenario.build();

    let face = scheme_face("My Tendrils Run Deep", MY_TENDRILS_RUN_DEEP);
    create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    // The precombat-main turn-based action sets the scheme in motion.
    drive_to_phase(&mut runner, Phase::PreCombatMain);
    let scheme = scheme_id(&runner, "My Tendrils Run Deep");
    assert!(
        !runner.state().objects[&scheme].face_down,
        "reach guard: the scheme must be face up before the end step"
    );
    (runner, scheme)
}

#[test]
fn tendrils_draws_two_and_abandons_with_six_lands() {
    let (mut runner, scheme) = setup_tendrils(6);
    assert_eq!(
        lands_on_battlefield(&runner, P0),
        6,
        "reach guard: six lands on the battlefield"
    );
    let hand_before = hand_size(&runner, P0);

    drive_to_phase(&mut runner, Phase::End);
    drain_triggers(&mut runner);

    assert_eq!(
        hand_size(&runner, P0),
        hand_before + 2,
        "the end-step trigger must draw two cards"
    );
    assert!(
        runner.state().objects[&scheme].face_down,
        "the chained tail must abandon the scheme"
    );
    assert_eq!(
        runner.state().scheme_deck.back(),
        Some(&scheme),
        "abandoned scheme must be on the bottom of the scheme deck"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn tendrils_stays_face_up_and_draws_nothing_with_five_lands() {
    let (mut runner, scheme) = setup_tendrils(5);
    assert_eq!(
        lands_on_battlefield(&runner, P0),
        5,
        "reach guard: five lands on the battlefield"
    );
    let hand_before = hand_size(&runner, P0);

    drive_to_phase(&mut runner, Phase::End);
    drain_triggers(&mut runner);

    assert_eq!(
        hand_size(&runner, P0),
        hand_before,
        "below the threshold the trigger must never fire"
    );
    assert!(
        !runner.state().objects[&scheme].face_down,
        "without the trigger the scheme must stay face up"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the end step must be quiescent"
    );
}

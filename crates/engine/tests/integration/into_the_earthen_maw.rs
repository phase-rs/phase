//! Into the Earthen Maw: three independent up-to-one exile legs.
//!
//! Oracle: "When you set this scheme in motion, exile up to one target
//! creature with flying, up to one target creature without flying, and all
//! cards from up to one target opponent's graveyard."
//!
//! CR 701.32a + CR 904.9: the set-in-motion trigger fires on turn 1.
//! CR 115.1 + CR 115.6: each "up to one target" is its own optional slot —
//! the flyer, the nonflyer, and the swept opponent are chosen independently.
//! CR 400.12: the third leg exiles every card in the CHOSEN opponent's
//! graveyard only; other players' graveyards are untouched.
//!
//! Revert-to-red: drop ZoneQual::TargetOpponent — the document gaps
//! (scheme_face asserts). Drop the chunk-loop mass carry-forward — leg 3
//! gaps the same way. Widen the sweep owner — P2's graveyard empties.
//! Skip a slot — the prompt count fails.

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const INTO_THE_EARTHEN_MAW: &str = "When you set this scheme in motion, exile up to one target creature with flying, up to one target creature without flying, and all cards from up to one target opponent's graveyard.";

const P2: PlayerId = PlayerId(2);

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
    synthesize_archenemy(&mut face);
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(gaps.is_empty(), "{name}: gaps: {gaps:?}");
    face
}

/// Drive to `target`, declaring no attackers/blockers through combat and
/// passing priority. Panics on any other prompt.
fn drive_to_phase(runner: &mut GameRunner, target: Phase) {
    for _ in 0..64 {
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

fn graveyard_names(runner: &GameRunner, player: PlayerId) -> Vec<String> {
    let mut names: Vec<String> = runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.zone == Zone::Graveyard && obj.owner == player)
        .map(|obj| obj.name.clone())
        .collect();
    names.sort();
    names
}

/// Build the 3-player game: P1 holds a flyer + a nonflyer on the
/// battlefield and two cards in the graveyard; P2 holds one graveyard card
/// (negative control). Returns the runner parked at precombat main with the
/// scheme face-up, plus the flyer, nonflyer, and P1's graveyard names.
fn setup_maw() -> (GameRunner, ObjectId, ObjectId, Vec<String>) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 3, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Library {i}"));
    }
    let flyer = scenario.add_creature(P1, "Cloud Drake", 2, 2).flying().id();
    let walker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    scenario.with_graveyard(P1, &["Dead Bear", "Dead Drake"]);
    scenario.with_graveyard(P2, &["Dead Opponent"]);
    let mut runner = scenario.build();

    let face = scheme_face("Into the Earthen Maw", INTO_THE_EARTHEN_MAW);
    let scheme = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down,
        "reach guard: turn 1 must set the scheme in motion"
    );
    assert_eq!(
        graveyard_names(&runner, P1),
        vec!["Dead Bear".to_string(), "Dead Drake".to_string()],
        "reach guard: P1's graveyard must start stocked"
    );
    (
        runner,
        flyer,
        walker,
        vec!["Dead Bear".into(), "Dead Drake".into()],
    )
}

/// Resolve the trigger, choosing `flyer_pick` / `walker_pick` /
/// `opponent_pick` (`None` declines that up-to-one slot). Returns the
/// number of target-selection prompts seen.
fn resolve_with_picks(
    runner: &mut GameRunner,
    flyer: ObjectId,
    walker: ObjectId,
    flyer_pick: bool,
    walker_pick: bool,
    opponent_pick: Option<PlayerId>,
) -> usize {
    let mut target_prompts = 0;
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
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                assert!(slot.optional, "every Maw leg is up-to-one (optional)");
                let legals = &slot.legal_targets;
                let choice = if legals.contains(&TargetRef::Object(flyer)) {
                    assert!(
                        !legals.contains(&TargetRef::Object(walker)),
                        "flyer slot must exclude the nonflyer"
                    );
                    flyer_pick.then_some(TargetRef::Object(flyer))
                } else if legals.contains(&TargetRef::Object(walker)) {
                    walker_pick.then_some(TargetRef::Object(walker))
                } else if legals.contains(&TargetRef::Player(P1)) {
                    assert!(
                        legals.contains(&TargetRef::Player(P2)),
                        "opponent slot must offer both opponents, got {legals:?}"
                    );
                    opponent_pick.map(TargetRef::Player)
                } else {
                    panic!("unexpected target slot legals: {legals:?}");
                };
                target_prompts += 1;
                runner
                    .act(GameAction::ChooseTarget { target: choice })
                    .expect("choose target must be accepted");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt resolving the maw: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
    target_prompts
}

#[test]
fn into_the_earthen_maw_exiles_all_three_legs() {
    let (mut runner, flyer, walker, _) = setup_maw();

    let target_prompts = resolve_with_picks(&mut runner, flyer, walker, true, true, Some(P1));
    assert_eq!(target_prompts, 3, "one prompt per up-to-one leg");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&flyer].zone,
        Zone::Exile,
        "the flyer must be exiled"
    );
    assert_eq!(
        runner.state().objects[&walker].zone,
        Zone::Exile,
        "the nonflyer must be exiled"
    );
    assert!(
        graveyard_names(&runner, P1).is_empty(),
        "P1's graveyard must be swept, got {:?}",
        graveyard_names(&runner, P1)
    );
    assert_eq!(
        graveyard_names(&runner, P2),
        vec!["Dead Opponent".to_string()],
        "the unchosen opponent's graveyard must be untouched"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn into_the_earthen_maw_decline_leaves_everything() {
    let (mut runner, flyer, walker, p1_graves) = setup_maw();

    let target_prompts = resolve_with_picks(&mut runner, flyer, walker, false, false, None);
    assert_eq!(target_prompts, 3, "declined legs must still prompt");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&flyer].zone,
        Zone::Battlefield,
        "declining must leave the flyer"
    );
    assert_eq!(
        runner.state().objects[&walker].zone,
        Zone::Battlefield,
        "declining must leave the nonflyer"
    );
    assert_eq!(
        graveyard_names(&runner, P1),
        p1_graves,
        "declining must leave P1's graveyard"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

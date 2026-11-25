//! My Wish Is Your Command: one free cast from the union of revealed hands.
//!
//! Oracle: "When you set this scheme in motion, each opponent reveals their
//! hand. You may choose a noncreature, nonland card revealed this way and
//! cast it without paying its mana cost."
//!
//! CR 701.32a + CR 904.9: the set-in-motion trigger reveals every hand.
//! CR 608.2c + CR 109.5: the controller makes ONE choice across the UNION
//! of revealed hands (not one per opponent) — the eligible pool spans
//! seats, filtered to noncreature nonland, and the pick casts free.
//!
//! Revert-to-red: drop the union branch — the document gaps (scheme_face
//! asserts). Fan the choice out per opponent — two prompts appear
//! (prompt-count fails) or the second seat's card is unpickable (pool
//! assertion fails). Skip the filter — the prompt offers four cards.

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const MY_WISH_IS_YOUR_COMMAND: &str = "When you set this scheme in motion, each opponent reveals their hand. You may choose a noncreature, nonland card revealed this way and cast it without paying its mana cost.";
const DIVINATION: &str = "Draw two cards.";

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
                runner
                    .pass_holding_team()
                    .expect("holding team passes together");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected prompt driving to {target:?}: {other:?}"),
        }
    }
    panic!("never reached {target:?}");
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.zone == Zone::Hand && obj.owner == player)
        .count()
}

/// Build the 3-player game: P1 holds a creature + a land (both excluded),
/// P2 holds Divination (eligible) + a land (excluded). Returns the runner
/// parked at precombat main with the scheme face-up, plus Divination's id.
fn setup_wish() -> (GameRunner, ObjectId) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 3, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
        scenario.add_card_to_library_top(P2, &format!("P2 Library {i}"));
    }
    scenario.add_creature_to_hand(P1, "Grizzly Bears", 2, 2);
    scenario.add_land_to_hand(P1, "Forest");
    let divination = scenario
        .add_spell_to_hand_from_oracle(P2, "Divination", false, DIVINATION)
        .id();
    scenario.add_land_to_hand(P2, "Island");
    let mut runner = scenario.build();

    let face = scheme_face("My Wish Is Your Command", MY_WISH_IS_YOUR_COMMAND);
    let scheme = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down,
        "reach guard: turn 1 must set the scheme in motion"
    );
    (runner, divination)
}

/// Resolve the trigger. When `accept` is true, take the optional union
/// choice and pick `pick`; otherwise decline the option (no choice, no
/// cast). Returns (optional prompts, reveal prompts) seen.
fn resolve_with_pick(runner: &mut GameRunner, accept: bool, pick: Vec<ObjectId>) -> (usize, usize) {
    let mut optional_prompts = 0;
    let mut reveal_prompts = 0;
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
                assert_eq!(player, P0, "the archenemy decides the may");
                optional_prompts += 1;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("may decision");
            }
            WaitingFor::RevealChoice {
                player,
                cards,
                optional,
                ..
            } => {
                assert_eq!(player, P0, "the archenemy makes the union choice");
                assert!(optional, "You may → the choice is optional");
                assert_eq!(
                    cards.len(),
                    1,
                    "only Divination survives the noncreature-nonland filter \
                     across both hands, got: {:?}",
                    cards
                        .iter()
                        .map(|id| runner.state().objects[id].name.clone())
                        .collect::<Vec<_>>()
                );
                reveal_prompts += 1;
                runner
                    .act(GameAction::SelectCards {
                        cards: pick.clone(),
                    })
                    .expect("union choice");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .pass_holding_team()
                    .expect("holding team passes together");
            }
            other => panic!("unexpected prompt resolving the wish: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
    (optional_prompts, reveal_prompts)
}

#[test]
fn my_wish_is_your_command_casts_union_pick_free() {
    let (mut runner, divination) = setup_wish();
    assert!(
        runner.state().objects[&divination].name == "Divination",
        "reach guard: Divination must start in P2's hand"
    );
    let p0_before = hand_size(&runner, P0);

    let (optional_prompts, reveal_prompts) = resolve_with_pick(&mut runner, true, vec![divination]);
    assert_eq!(optional_prompts, 1, "the may must offer once");
    assert_eq!(
        reveal_prompts, 1,
        "exactly one union choice across both hands"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        hand_size(&runner, P0),
        p0_before + 2,
        "the free Divination must resolve and draw two"
    );
    assert_eq!(
        runner.state().objects[&divination].zone,
        Zone::Graveyard,
        "the cast spell must reach the graveyard"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

#[test]
fn my_wish_is_your_command_decline_casts_nothing() {
    let (mut runner, divination) = setup_wish();
    let p0_before = hand_size(&runner, P0);

    let (optional_prompts, reveal_prompts) = resolve_with_pick(&mut runner, false, vec![]);
    assert_eq!(optional_prompts, 1, "the may must offer once");
    assert_eq!(reveal_prompts, 0, "declining must skip the pick");

    assert_eq!(
        hand_size(&runner, P0),
        p0_before,
        "declining must draw nothing"
    );
    assert_eq!(
        runner.state().objects[&divination].zone,
        Zone::Hand,
        "declining must leave Divination in P2's hand"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

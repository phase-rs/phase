//! Grenzo, Dungeon Warden (issue #8989): "{2}: Put the bottom card of your
//! library into your graveyard. If it's a creature card with power less than or
//! equal to Grenzo's power, put it onto the battlefield."
//!
//! Before the fix the first clause lowered to a generic library→graveyard
//! `ChangeZone`, so the engine offered a choose-ANY-card prompt over the library,
//! and the rider lowered to `Unimplemented`. These tests drive the real
//! activation pipeline with the printed Oracle text and assert that the bottom
//! card is taken without an interactive choice, in each branch of the rider.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim printed Oracle text (Scryfall). A paraphrase can reach a different
/// parser branch and go green while the real card stays broken.
const GRENZO: &str = "Grenzo enters with X +1/+1 counters on it.\n{2}: Put the bottom card of your library into your graveyard. If it's a creature card with power less than or equal to Grenzo's power, put it onto the battlefield.";

struct GrenzoRun {
    runner: GameRunner,
    bottom: ObjectId,
    top: ObjectId,
    events: Vec<GameEvent>,
}

/// Stage a library whose BOTTOMMOST card is `bottom` (creature with the given
/// power/toughness, or a plain noncreature card when `None`) under `filler_count`
/// other cards, then activate Grenzo's `{2}` ability through the real pipeline.
///
/// Any window other than priority/mana payment fails the test: the pre-fix
/// `ChangeZone` lowered to a card-selection prompt over the whole library.
fn activate_grenzo(bottom_creature: Option<(i32, i32)>, filler_count: usize) -> GrenzoRun {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    let grenzo = scenario
        .add_creature_from_oracle(P0, "Grenzo, Dungeon Warden", 2, 2, GRENZO)
        .id();

    // `add_*_to_library_top` seats each new card at index 0 (CR 401.2: the top),
    // so adding the bottom card first leaves it beneath every filler.
    let bottom = match bottom_creature {
        Some(_) => scenario
            .add_spell_to_library_top(P0, "Bottom Creature", false)
            .as_creature()
            .id(),
        None => scenario.add_card_to_library_top(P0, "Bottom Noncreature"),
    };
    let mut top = bottom;
    for i in 0..filler_count {
        top = scenario.add_card_to_library_top(P0, &format!("Filler {i}"));
    }

    let mut runner = scenario.build();
    if let Some((power, toughness)) = bottom_creature {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&bottom)
            .expect("staged bottom card");
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);
    }
    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: grenzo,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Grenzo's {2} ability must be activatable")
        .events;

    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("pass priority")
                        .events,
                );
            }
            // The {2} is funded above; confirm the automatic payment.
            WaitingFor::ManaPayment { .. } => {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("pay {2} from the pool")
                        .events,
                );
            }
            other => panic!(
                "Grenzo's activation must take the bottom card without an \
                 interactive choice, got {other:?}"
            ),
        }
    }

    GrenzoRun {
        runner,
        bottom,
        top,
        events,
    }
}

#[test]
fn grenzo_puts_an_eligible_bottom_creature_onto_the_battlefield() {
    let run = activate_grenzo(Some((2, 2)), 3);

    // CR 401.2: the bottom card — not the top filler — is the one the ability
    // moved, and the printed rider put it back onto the battlefield.
    assert_eq!(
        run.runner.state().objects[&run.top].zone,
        Zone::Library,
        "the top card must stay in the library"
    );
    assert_eq!(
        run.runner.state().objects[&run.bottom].zone,
        Zone::Battlefield,
        "a creature with power <= Grenzo's power must enter the battlefield"
    );
    assert_eq!(run.runner.state().objects[&run.bottom].controller, P0);
    assert!(
        run.runner.state().players[0].graveyard.is_empty(),
        "the eligible card must leave the graveyard for the battlefield"
    );
    assert!(
        matches!(run.runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the activation must resolve back to priority"
    );
    // CR 701.17a: putting the bottom card into a graveyard is not milling, so no
    // mill trigger may observe it.
    assert!(
        !run.events
            .iter()
            .any(|event| matches!(event, GameEvent::Milled { .. })),
        "a bottom-edge move must not publish Milled, got {:?}",
        run.events
    );
}

#[test]
fn grenzo_leaves_an_overpowered_bottom_creature_in_the_graveyard() {
    let run = activate_grenzo(Some((3, 3)), 2);

    assert_eq!(
        run.runner.state().objects[&run.bottom].zone,
        Zone::Graveyard,
        "power 3 is greater than Grenzo's power 2, so the card stays in the graveyard"
    );
    assert_eq!(
        run.runner.state().objects[&run.top].zone,
        Zone::Library,
        "the top card must stay in the library"
    );
}

#[test]
fn grenzo_leaves_a_noncreature_bottom_card_in_the_graveyard() {
    let run = activate_grenzo(None, 2);

    assert_eq!(
        run.runner.state().objects[&run.bottom].zone,
        Zone::Graveyard,
        "a noncreature bottom card stays in the graveyard"
    );
    assert_eq!(
        run.runner.state().objects[&run.top].zone,
        Zone::Library,
        "the top card must stay in the library"
    );
}

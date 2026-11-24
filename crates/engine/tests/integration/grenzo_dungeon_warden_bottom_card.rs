//! Grenzo, Dungeon Warden (issue #8989): "{2}: Put the bottom card of your
//! library into your graveyard. If it's a creature card with power less than or
//! equal to Grenzo's power, put it onto the battlefield."
//!
//! Before the fix the first clause lowered to a generic library→graveyard
//! `ChangeZone`, so the engine offered a choose-ANY-card prompt over the library,
//! and the rider lowered to `Unimplemented`. These tests drive the real
//! activation pipeline with the printed Oracle text and assert that the bottom
//! card is taken without an interactive choice, in each branch of the rider.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim printed Oracle text (Scryfall). A paraphrase can reach a different
/// parser branch and go green while the real card stays broken.
const GRENZO: &str = "Grenzo enters with X +1/+1 counters on it.\n{2}: Put the bottom card of your library into your graveyard. If it's a creature card with power less than or equal to Grenzo's power, put it onto the battlefield.";

const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const LEYLINE_OF_THE_VOID: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.";

/// A plain removal spell: Grenzo must be put into the graveyard in response to
/// its own activation. Murder is "Destroy target creature." for {1}{B}{B}.
const MURDER: &str = "Destroy target creature.";

struct GrenzoRun {
    runner: GameRunner,
    bottom: ObjectId,
    top: ObjectId,
    events: Vec<GameEvent>,
}

/// The staged board for one Grenzo activation: `grenzo` on the battlefield with
/// `grenzo_power` as its current power, `bottom` as the BOTTOMMOST library card
/// (a creature with the given power/toughness, or a plain noncreature card when
/// `None`) under `filler_count` other cards, and a Lightning Bolt in hand.
struct GrenzoSetup {
    runner: GameRunner,
    grenzo: ObjectId,
    bottom: ObjectId,
    top: ObjectId,
    removal: ObjectId,
}

/// Stage the board under `grenzo_power` as Grenzo's CURRENT power (the printed
/// base stays 2, so a buff and the card's base characteristics differ).
///
/// `add_*_to_library_top` seats each new card at index 0 (CR 401.2: the top), so
/// adding the bottom card first leaves it beneath every filler.
fn stage_grenzo(
    bottom_creature: Option<(i32, i32)>,
    filler_count: usize,
    grenzo_power: i32,
) -> GrenzoSetup {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ],
    );
    let grenzo = scenario
        .add_creature_from_oracle(P0, "Grenzo, Dungeon Warden", 2, 2, GRENZO)
        .id();
    // `grenzo_power` is Grenzo's CURRENT power, staged as +1/+1 counters (layer
    // 7d) so the layer pass keeps it. The printed base power stays 2 — that is
    // what the departed graveyard card reads, while its last-known power is the
    // buffed value.
    if grenzo_power > 2 {
        scenario.with_counter(grenzo, CounterType::Plus1Plus1, (grenzo_power - 2) as u32);
    }
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 1,
        })
        .id();

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
    GrenzoSetup {
        runner,
        grenzo,
        bottom,
        top,
        removal,
    }
}

/// Activate Grenzo's `{2}` ability and stop at priority with it on the stack, so
/// a test can respond before it resolves.
///
/// Any window other than priority/mana payment fails: the pre-fix `ChangeZone`
/// lowered to a card-selection prompt over the whole library.
fn activate_onto_stack(runner: &mut GameRunner, grenzo: ObjectId) -> Vec<GameEvent> {
    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: grenzo,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Grenzo's {2} ability must be activatable")
        .events;
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => break,
            // The {2} is funded by the caller; confirm the automatic payment.
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
    assert!(
        !runner.state().stack.is_empty(),
        "the activation is on the stack, not resolved"
    );
    events
}

fn activate_grenzo(bottom_creature: Option<(i32, i32)>, filler_count: usize) -> GrenzoRun {
    let GrenzoSetup {
        mut runner,
        grenzo,
        bottom,
        top,
        ..
    } = stage_grenzo(bottom_creature, filler_count, 2);
    let mut events = activate_onto_stack(&mut runner, grenzo);

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

/// CR 608.2h: "power less than or equal to Grenzo's power" reads the power the
/// source had in the zone the ability expects it in. Grenzo is a 4-power
/// creature (a buff over its printed 2) when it is removed in response; the
/// ability still resolves and must compare the bottom card against the
/// last-known power (4), not the base power of the graveyard card (2).
#[test]
fn grenzo_uses_last_known_power_after_dying_in_response() {
    let GrenzoSetup {
        mut runner,
        grenzo,
        bottom,
        top,
        removal,
    } = stage_grenzo(Some((3, 3)), 2, 4);
    let _ = activate_onto_stack(&mut runner, grenzo);
    assert_eq!(
        runner.state().objects[&grenzo].power,
        Some(4),
        "reach: Grenzo is a 4-power creature when the ability is activated"
    );

    let _ = runner.cast(removal).target_objects(&[grenzo]).commit();
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&grenzo].zone,
        Zone::Graveyard,
        "reach: the removal spell killed Grenzo before the ability resolved"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Grenzo's ability is still on the stack"
    );
    assert_eq!(
        runner.state().objects[&bottom].zone,
        Zone::Library,
        "reach: the bottom card has not moved yet"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&bottom].zone,
        Zone::Battlefield,
        "power 3 <= Grenzo's last-known power 4; the graveyard card's base power 2 must not be used"
    );
    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Library,
        "the top card must stay in the library"
    );
}

/// CR 616.1 + CR 400.7j (issue #8989 F1): When the bottom card's graveyard move
/// is redirected to Exile by Rest in Peace and Leyline of the Void (requiring a
/// replacement choice under CR 616.1), answering the choice must not lose the
/// moved object. Grenzo's rider must find the creature in exile and put it onto
/// the battlefield, emitting no `Milled` event.
#[test]
fn grenzo_puts_eligible_creature_onto_battlefield_after_replacement_choice() {
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

    // P1 controls both Rest in Peace and Leyline of the Void.
    scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    scenario.add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE_OF_THE_VOID);

    let bottom = scenario
        .add_spell_to_library_top(P0, "Grizzly Bears", false)
        .as_creature()
        .id();
    let mut top = bottom;
    for i in 0..2 {
        top = scenario.add_card_to_library_top(P0, &format!("Filler {i}"));
    }

    let mut runner = scenario.build();

    let obj = runner
        .state_mut()
        .objects
        .get_mut(&bottom)
        .expect("staged bottom card");
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);

    // Drain any opening triggers (such as Rest in Peace's ETB)
    runner.advance_until_stack_empty();

    let mut events = activate_onto_stack(&mut runner, grenzo);

    // Pass priority to begin resolving Grenzo's ability
    let mut saw_replacement_choice = false;
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
            WaitingFor::ManaPayment { .. } => {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("pay mana")
                        .events,
                );
            }
            WaitingFor::ReplacementChoice { .. } => {
                saw_replacement_choice = true;
                events.extend(
                    runner
                        .act(GameAction::ChooseReplacement { index: 0 })
                        .expect("choose replacement")
                        .events,
                );
            }
            other => panic!("unexpected waiting_for {other:?}"),
        }
    }

    assert!(
        saw_replacement_choice,
        "the two graveyard replacements must trigger a replacement choice"
    );

    // CR 400.7j: Grizzly Bears was exiled by the replacement, but Grenzo's rider
    // finds it in exile and puts it onto the battlefield.
    assert_eq!(
        runner.state().objects[&bottom].zone,
        Zone::Battlefield,
        "the eligible creature must enter the battlefield from exile"
    );
    assert_eq!(
        runner.state().objects[&bottom].controller,
        P0,
        "the creature must enter under Grenzo controller's control"
    );
    assert!(
        runner.state().players[0].graveyard.is_empty(),
        "P0's graveyard must be empty"
    );
    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Library,
        "the top filler card must remain in the library"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the resolution must finish back at priority"
    );
    // CR 701.17a: bottom card move is not a mill, so no Milled event may be emitted.
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::Milled { .. })),
        "a bottom-edge move must not publish Milled"
    );
}

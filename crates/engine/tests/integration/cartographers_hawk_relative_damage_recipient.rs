//! Issue #8391 — Cartographer's Hawk checks the damaged player's land count
//! when combat damage is dealt, not while its triggered ability resolves.

use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{Effect, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::run_combat;

const CARTOGRAPHERS_HAWK_ORACLE: &str = "Flying\nWhen this creature deals combat damage to a player who controls more lands than you, return it to its owner's hand. If you do, you may search your library for a Plains card, put it onto the battlefield tapped, then shuffle.";

struct HawkBoard {
    runner: GameRunner,
    hawk: ObjectId,
    defender_lands: Vec<ObjectId>,
    plains_in_library: ObjectId,
}

fn add_plains_to_library(runner: &mut GameRunner) -> ObjectId {
    let state = runner.state_mut();
    let plains = create_object(
        state,
        CardId(state.next_object_id),
        P0,
        "Plains".to_string(),
        Zone::Library,
    );
    let object = state
        .objects
        .get_mut(&plains)
        .expect("created library Plains must exist");
    object.card_types.core_types.push(CoreType::Land);
    object.card_types.supertypes.push(Supertype::Basic);
    object.card_types.subtypes.push("Plains".to_string());
    object.base_card_types = object.card_types.clone();
    plains
}

fn board(controller_lands: usize, defender_lands: usize, defender_nonlands: usize) -> HawkBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hawk = scenario
        .add_creature(P0, "Cartographer's Hawk", 2, 2)
        .from_oracle_text(CARTOGRAPHERS_HAWK_ORACLE)
        .id();
    for _ in 0..controller_lands {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    let mut defender_land_ids = Vec::new();
    for _ in 0..defender_lands {
        defender_land_ids.push(scenario.add_basic_land(P1, ManaColor::Green));
    }
    for index in 0..defender_nonlands {
        scenario.add_creature(P1, &format!("Defender nonland {index}"), 2, 2);
    }

    let mut runner = scenario.build();
    let plains_in_library = add_plains_to_library(&mut runner);
    HawkBoard {
        runner,
        hawk,
        defender_lands: defender_land_ids,
        plains_in_library,
    }
}

/// Resolve the already-created Hawk trigger. The optional search is accepted
/// and the explicitly supplied Plains is selected, making every continuation
/// in the printed effect chain reachable.
fn resolve_hawk_trigger(runner: &mut GameRunner, plains: ObjectId) {
    for _ in 0..24 {
        runner.advance_until_stack_empty();
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting Cartographer's Hawk's optional search must succeed");
            }
            WaitingFor::SearchChoice { cards, .. } => {
                assert!(
                    cards.contains(&plains),
                    "the Plains in the library must be a legal search choice: {cards:?}"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![plains],
                    })
                    .expect("selecting the searched Plains must succeed");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            other => panic!("unexpected state while resolving Cartographer's Hawk: {other:?}"),
        }
    }
    panic!("Cartographer's Hawk trigger did not finish resolving");
}

/// CR 603.2: equal land counts do not satisfy the event predicate. The second
/// board keeps the same equal LAND census while giving the defender extra
/// nonland permanents, so a broad or all-permanents count cannot pass.
#[test]
fn cartographers_hawk_does_not_trigger_for_equal_lands_even_with_extra_nonlands() {
    for (name, defender_nonlands) in [("equal lands", 0), ("hostile nonland census", 4)] {
        let HawkBoard {
            mut runner, hawk, ..
        } = board(2, 2, defender_nonlands);
        run_combat(&mut runner, vec![hawk], vec![]);
        assert!(
            runner.state().stack.is_empty(),
            "{name}: equal land counts must not put Hawk's trigger on the stack"
        );
        assert_eq!(
            runner.state().objects[&hawk].zone,
            Zone::Battlefield,
            "{name}: Hawk must remain on the battlefield when its event predicate is false"
        );
    }
}

/// CR 603.2 checks this recipient predicate when combat damage happens. The
/// defender's excess land is then removed before the trigger resolves; Hawk
/// still returns and its optional Plains search proceeds, proving this is not a
/// CR 603.4 intervening-if condition rechecked at resolution.
#[test]
fn cartographers_hawk_uses_the_damaged_players_land_count_at_event_time() {
    let HawkBoard {
        mut runner,
        hawk,
        defender_lands,
        plains_in_library,
    } = board(1, 2, 0);
    run_combat(&mut runner, vec![hawk], vec![]);
    assert!(
        !runner.state().stack.is_empty(),
        "strictly more defender lands must create Hawk's combat-damage trigger"
    );

    let excess_land = defender_lands[0];
    let destroy = ResolvedAbility::new(
        Effect::Destroy {
            target: TargetFilter::Any,
            cant_regenerate: false,
        },
        vec![TargetRef::Object(excess_land)],
        hawk,
        P0,
    );
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(runner.state_mut(), &destroy, &mut events, 0)
        .expect("removing the defender's excess land must resolve");
    assert_eq!(runner.state().objects[&excess_land].zone, Zone::Graveyard);

    resolve_hawk_trigger(&mut runner, plains_in_library);
    assert_eq!(
        runner.state().objects[&hawk].zone,
        Zone::Hand,
        "Hawk must still bounce after the defender's count falls before resolution"
    );
    assert_eq!(
        runner.state().objects[&plains_in_library].zone,
        Zone::Battlefield
    );
    assert!(
        runner.state().objects[&plains_in_library].tapped,
        "the searched Plains must enter tapped"
    );
}

//! Path of Discovery {3}{G} Enchantment — "Whenever a creature you control
//! enters, it explores."
//!
//! CR 603.6a: each time an event puts one or more permanents onto the
//! battlefield, every permanent on the battlefield is checked for matching
//! enters-the-battlefield triggers. CR 701.44a: to explore, the exploring
//! permanent's controller reveals the top card of their library; a revealed
//! land goes to hand, otherwise a +1/+1 counter goes on the exploring
//! permanent and the player may put the revealed card into their graveyard.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Path of Discovery {3}{G} Enchantment — verbatim Oracle text.
const PATH_OF_DISCOVERY: &str = "Whenever a creature you control enters, it explores.";

fn board() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Path of Discovery", PATH_OF_DISCOVERY);
    scenario
}

/// CR 701.44a (land branch): the entering creature explores, the revealed
/// land is put into its controller's hand, and no counter is placed.
#[test]
fn path_of_discovery_explore_puts_revealed_land_into_hand() {
    let mut scenario = board();
    let land = scenario.add_land_to_library_top(P0, "Forest").id();
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(bear).resolve();

    assert_eq!(
        outcome.zone_of(bear),
        Zone::Battlefield,
        "reach: the cast creature entered the battlefield"
    );
    assert_eq!(
        outcome.zone_of(land),
        Zone::Hand,
        "explore must put the revealed land into its controller's hand"
    );
    assert_eq!(
        outcome.counters(bear, CounterType::Plus1Plus1),
        0,
        "a land reveal puts no counter on the exploring creature"
    );
    // DISCRIMINATING: the explore completion names the entering creature as
    // the explorer. Pre-fix the chain fell back to the trigger source, so
    // this named Path of Discovery instead of the bear.
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Explore,
                source_id,
                ..
            } if *source_id == bear
        )),
        "the explore completion must name the entering creature, events: {:?}",
        outcome.events()
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the trigger must resolve fully, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        outcome.state().stack.is_empty(),
        "the stack must be empty after the trigger resolves"
    );
}

/// CR 701.44a (nonland branch): the entering creature explores, gets a
/// +1/+1 counter, and its controller chooses the revealed card's disposition.
#[test]
fn path_of_discovery_explore_counters_on_nonland_then_graveyard_choice() {
    let mut scenario = board();
    let bolt = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .id();
    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(bear).resolve();

    assert_eq!(
        outcome.zone_of(bear),
        Zone::Battlefield,
        "reach: the cast creature entered the battlefield"
    );
    assert_eq!(
        outcome.counters(bear, CounterType::Plus1Plus1),
        1,
        "a nonland reveal must put a +1/+1 counter on the exploring creature"
    );
    match outcome.final_waiting_for() {
        WaitingFor::DigChoice { cards, player, .. } => {
            assert_eq!(*player, P0);
            assert_eq!(
                cards.as_slice(),
                &[bolt],
                "explore must offer the revealed card's disposition"
            );
        }
        other => panic!("expected the explore DigChoice, got {other:?}"),
    }

    // CR 701.44a: the player may put the revealed card into their graveyard.
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining the put-back is legal");
    let state = runner.state();
    assert_eq!(state.objects[&bolt].zone, Zone::Graveyard);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty(),
        "resolution must settle after the disposition, got {:?}",
        state.waiting_for
    );
}

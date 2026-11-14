//! Call to the Kindred's upkeep look, and Tibalt, Wicked Tormentor's nested
//! "If you do" as a control, driven through the public action pipeline.

use std::collections::HashSet;

use engine::game::effects::attach;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const CALL_TO_THE_KINDRED: &str = "Enchant creature\nAt the beginning of your upkeep, you may \
look at the top five cards of your library. If you do, you may put a creature card that shares \
a creature type with enchanted creature from among them onto the battlefield, then you put the \
rest of those cards on the bottom of your library in any order.";

/// P1 controls Call to the Kindred on Fyndhorn Elves (Elf Druid); `library`
/// is P1's library, top first. The runner stops at P1's upkeep.
fn kindred_at_upkeep(library: &[&str]) -> (GameRunner, Vec<ObjectId>) {
    let db =
        shared_card_db().expect("Call to the Kindred tests require the integration card database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_real_card(P1, "Fyndhorn Elves", Zone::Battlefield, db);
    let aura = scenario
        .add_enchantment_from_oracle(P1, "Call to the Kindred", CALL_TO_THE_KINDRED)
        .with_subtypes(vec!["Aura"])
        .id();
    let library = library
        .iter()
        .map(|name| scenario.add_real_card(P1, name, Zone::Library, db))
        .collect();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types =
        ["Elf", "Druid", "Shaman", "Goblin", "Berserker", "Warrior"]
            .map(String::from)
            .to_vec();
    attach::attach_to(runner.state_mut(), aura, host);
    runner.advance_to_phase(Phase::Upkeep);
    (runner, library)
}

fn library_of(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner.state().players[player.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

fn set(ids: &[ObjectId]) -> HashSet<ObjectId> {
    ids.iter().copied().collect()
}

/// Passes priority until the top of the stack resolves into a prompt.
fn resolve_stack_to_prompt(runner: &mut GameRunner) -> WaitingFor {
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || runner.state().stack.is_empty()
        {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    runner.state().waiting_for.clone()
}

/// Accepts the look and returns the `DigChoice` it opens.
fn accept_look(runner: &mut GameRunner) -> WaitingFor {
    let waiting = resolve_stack_to_prompt(runner);
    assert!(
        matches!(waiting, WaitingFor::OptionalEffectChoice { player: P1, .. }),
        "the look is P1's optional choice, got {waiting:?}"
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the look");
    runner.state().waiting_for.clone()
}

fn assert_resolved(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution completes, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
}

const ELF_LOOK: [&str; 8] = [
    "Llanowar Elves",
    "Raging Goblin",
    "Elvish Mystic",
    "Forest",
    "Island",
    "Goblin Piker",
    "Elvish Visionary",
    "Forest",
];

/// CR 608.2d: "you may put a creature card" offers the looked-at creatures
/// sharing a type with the enchanted creature, puts the chosen one onto the
/// battlefield, and the rest of the five go to the bottom.
#[test]
fn accepted_look_puts_one_sharing_creature_and_bottoms_the_rest() {
    let (mut runner, lib) = kindred_at_upkeep(&ELF_LOOK);
    let [llanowar, goblin, mystic, forest, island, ..] = lib[..] else {
        unreachable!()
    };
    let WaitingFor::DigChoice {
        player,
        cards,
        keep_count,
        up_to,
        selectable_cards,
        ..
    } = accept_look(&mut runner)
    else {
        panic!(
            "the accepted look opens a DigChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1);
    assert_eq!(set(&cards), set(&lib[..5]));
    assert_eq!((keep_count, up_to), (1, true));
    assert_eq!(set(&selectable_cards), set(&[llanowar, mystic]));

    runner
        .act(GameAction::SelectCards {
            cards: vec![llanowar],
        })
        .expect("put Llanowar Elves");

    assert_resolved(&runner);
    assert_eq!(runner.state().objects[&llanowar].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&mystic].zone, Zone::Library);
    let after = library_of(&runner, P1);
    assert_eq!(after[..3], lib[5..]);
    // CR 401.4: the rest are arranged by their owner; only the set is pinned.
    assert_eq!(set(&after[3..]), set(&[goblin, mystic, forest, island]));
}

/// CR 118.12 + CR 608.2c: putting no creature still moves all five looked-at
/// cards to the bottom.
#[test]
fn accepted_look_with_no_creature_put_bottoms_all_five() {
    let (mut runner, lib) = kindred_at_upkeep(&ELF_LOOK);
    let WaitingFor::DigChoice {
        selectable_cards, ..
    } = accept_look(&mut runner)
    else {
        panic!(
            "the accepted look opens a DigChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(set(&selectable_cards), set(&[lib[0], lib[2]]));

    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("put nothing");

    assert_resolved(&runner);
    let after = library_of(&runner, P1);
    assert_eq!(after[..3], lib[5..]);
    assert_eq!(set(&after[3..]), set(&lib[..5]));
}

/// With no looked-at card sharing a creature type, the choice is empty and all
/// five still go to the bottom.
#[test]
fn accepted_look_with_no_sharing_creature_bottoms_all_five() {
    let (mut runner, lib) = kindred_at_upkeep(&[
        "Raging Goblin",
        "Forest",
        "Goblin Piker",
        "Island",
        "Forest",
        "Llanowar Elves",
        "Island",
    ]);
    let WaitingFor::DigChoice {
        cards,
        selectable_cards,
        ..
    } = accept_look(&mut runner)
    else {
        panic!(
            "the accepted look opens a DigChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(set(&cards), set(&lib[..5]));
    assert!(selectable_cards.is_empty());

    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("put nothing");

    assert_resolved(&runner);
    let after = library_of(&runner, P1);
    assert_eq!(after[..2], lib[5..]);
    assert_eq!(set(&after[2..]), set(&lib[..5]));
}

/// CR 118.12: declining the look skips the whole "If you do" body.
#[test]
fn declined_look_leaves_the_library_untouched() {
    let (mut runner, lib) = kindred_at_upkeep(&ELF_LOOK);
    let waiting = resolve_stack_to_prompt(&mut runner);
    assert!(
        matches!(waiting, WaitingFor::OptionalEffectChoice { player: P1, .. }),
        "the look is P1's optional choice, got {waiting:?}"
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline the look");

    assert_resolved(&runner);
    assert_eq!(library_of(&runner, P1), lib);
}

/// Tibalt's second +1 against P1's creature, with P1 taking the damage, stops
/// at P0's optional discard. Returns the discard, the kept card and the draw.
fn tibalt_at_optional_discard() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let db = shared_card_db().expect("Tibalt tests require the integration card database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tibalt = scenario.add_real_card(P0, "Tibalt, Wicked Tormentor", Zone::Battlefield, db);
    let victim = scenario.add_real_card(P1, "Goblin Piker", Zone::Battlefield, db);
    let fodder = scenario.add_real_card(P0, "Forest", Zone::Hand, db);
    let kept = scenario.add_real_card(P0, "Raging Goblin", Zone::Hand, db);
    let draw = scenario.add_real_card(P0, "Island", Zone::Library, db);
    scenario.add_real_card(P0, "Island", Zone::Library, db);
    let mut runner = scenario.build();

    let outcome = runner
        .activate(tibalt, 1)
        .target_object(victim)
        .stop_at_optional()
        .resolve();
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::UnlessPayment { player: P1, .. }
        ),
        "the victim's controller chooses, got {:?}",
        outcome.final_waiting_for()
    );
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("P1 takes the damage");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "\"you may discard a card\" is P0's choice, got {:?}",
        runner.state().waiting_for
    );
    (runner, fodder, kept, draw)
}

/// CR 118.12: Tibalt's "If you do, draw a card" reads the discard choice.
#[test]
fn tibalt_discard_accepted_draws_a_card() {
    let (mut runner, fodder, kept, draw) = tibalt_at_optional_discard();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("discard");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DiscardChoice { player: P0, .. }
        ),
        "P0 picks the discard, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("discard the Forest");
    assert_resolved(&runner);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&kept].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&draw].zone, Zone::Hand);
}

/// CR 118.12: declining Tibalt's discard skips the draw.
#[test]
fn tibalt_discard_declined_draws_nothing() {
    let (mut runner, fodder, kept, draw) = tibalt_at_optional_discard();
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("keep the hand");
    assert_resolved(&runner);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&kept].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&draw].zone, Zone::Library);
}

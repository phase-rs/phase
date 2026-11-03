//! The Majestic Duo — "When The Majestic Duo enters, if you don't control
//! another permanent named The Majestic Duo, create a token that's a copy of
//! it, except it's not legendary, …"
//!
//! CR 603.4: intervening-if — checked when the trigger would fire and again on
//! resolution. CR 201.2: "named The Majestic Duo" is the literal card name; it
//! ends at the comma that opens the effect. When the name swallowed
//! ", create a token …" the copy effect was lost and nothing was created.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim Oracle text (card-data.json), first ability only — the combat
// ability is irrelevant here.
const MAJESTIC_DUO: &str = "When The Majestic Duo enters, if you don't control another permanent named The Majestic Duo, create a token that's a copy of it, except it's not legendary, it has \"Whenever this creature deals combat damage to a player, draw a card and earnestly tell them good luck,\" and it loses all other abilities.";

fn duos_on_battlefield(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield && o.controller == P0 && o.name == "The Majestic Duo"
        })
        .count()
}

fn cast_duo(existing_duo: bool) -> (usize, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if existing_duo {
        // Stands in for the non-legendary token copy the first Duo made.
        scenario.add_creature(P0, "The Majestic Duo", 3, 3);
    }
    let duo: ObjectId = scenario
        .add_creature_to_hand_from_oracle(P0, "The Majestic Duo", 3, 3, MAJESTIC_DUO)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::White],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]); 3],
    );
    let mut runner = scenario.build();
    let before = duos_on_battlefield(&runner);
    let outcome = runner.cast(duo).resolve();
    assert_eq!(outcome.zone_of(duo), Zone::Battlefield);
    runner.advance_until_stack_empty();
    (before, duos_on_battlefield(&runner))
}

#[test]
fn majestic_duo_alone_creates_its_copy() {
    let (before, after) = cast_duo(false);
    assert_eq!(before, 0);
    assert_eq!(after, 2, "the Duo plus its token copy");
}

#[test]
fn majestic_duo_with_another_duo_creates_nothing() {
    let (before, after) = cast_duo(true);
    assert_eq!(before, 1);
    assert_eq!(after, 2, "the existing Duo plus the new one, and no copy");
}

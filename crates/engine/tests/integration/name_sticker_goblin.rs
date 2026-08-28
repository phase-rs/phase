//! Real-card coverage for "Name Sticker" Goblin's intervening-if d20 table.
//!
//! The card is loaded from the generated integration fixture, so this proves
//! the production parser output rather than a hand-written ability. The source
//! contraction, named/controller-scoped cap, and ASCII range rows are all
//! exercised together through a cast and ETB resolution.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::events::GameEvent;
use engine::types::game_state::StackEntryKind;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

use crate::support::shared_card_db;

const CARD: &str = "\"Name Sticker\" Goblin";

fn red_mana(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect()
}

fn mana_red_count(state: &engine::types::game_state::GameState) -> usize {
    state.players[P0.0 as usize]
        .mana_pool
        .mana
        .iter()
        .filter(|unit| unit.color == ManaType::Red)
        .count()
}

/// Cast one real Goblin after seeding pre-existing copies. The RNG is reset
/// only after the spell is committed and immediately before resolution, making
/// the die face a stable property of the documented seed rather than setup
/// activity that may evolve independently of this card.
fn cast_name_sticker_goblin(
    controller_copies_before_cast: usize,
    seed: u64,
) -> engine::game::scenario::CastOutcome {
    let db = shared_card_db().expect("generated integration card fixture must load");
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    for _ in 0..controller_copies_before_cast {
        scenario.add_real_card(P0, CARD, Zone::Battlefield, db);
    }
    // Hostile fixtures prove both count-filter axes: P1's same-named Goblin
    // cannot count for "you control", and P0's other creatures cannot count
    // merely because they are creatures. With eight P0 named copies, the
    // entering ninth still qualifies despite both hostile groups.
    scenario.add_real_card(P1, CARD, Zone::Battlefield, db);
    for _ in 0..9 {
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    }
    let entering = scenario.add_real_card(P0, CARD, Zone::Hand, db);
    scenario.with_mana_pool(P0, red_mana(3));

    let mut runner = scenario.build();
    let mut committed = runner.cast(entering).commit();
    let state = committed.state_mut();
    state.rng_seed = seed;
    state.rng_word_pos = 0;
    state.rng = ChaCha20Rng::seed_from_u64(seed);
    committed.resolve()
}

#[test]
fn name_sticker_goblin_real_card_pays_each_d20_band_with_controller_scoped_cap() {
    // These faces are asserted from the emitted event, not inferred from a
    // branch. They pin all three d20 rows after resetting RNG immediately before
    // resolution: seed 6 -> 1, seed 0 -> 11, seed 15 -> 16.
    for (seed, expected_roll, expected_red) in [(6, 1, 4), (0, 11, 5), (15, 16, 6)] {
        let outcome = cast_name_sticker_goblin(8, seed);
        let rolled = outcome.events().iter().find_map(|event| match event {
            GameEvent::DieRolled {
                sides: 20,
                result: Some(result),
                ..
            } => Some(*result),
            _ => None,
        });
        assert_eq!(
            rolled,
            Some(expected_roll),
            "seed {seed} must reach its pinned d20 band"
        );
        assert_eq!(
            mana_red_count(outcome.state()),
            expected_red,
            "seed {seed} / d20 {expected_roll} must add exactly {expected_red} red mana"
        );
    }
}

#[test]
fn name_sticker_goblin_cap_blocks_tenth_controller_copy_even_with_hostile_copy() {
    // Nine P0 copies plus the entering one make ten creatures the controller
    // controls. P1's same-named Goblin is deliberately present to distinguish
    // controller scope from a global named-card count.
    let outcome = cast_name_sticker_goblin(9, 0);
    assert!(
        !outcome
            .events()
            .iter()
            .any(|event| matches!(event, GameEvent::DieRolled { sides: 20, .. })),
        "the tenth controlled copy must not roll a d20"
    );
    assert_eq!(
        mana_red_count(outcome.state()),
        0,
        "the false intervening-if must add no table mana"
    );
}

#[test]
fn name_sticker_goblin_rechecks_battlefield_intervening_if_at_resolution() {
    let db = shared_card_db().expect("generated integration card fixture must load");
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let entering = scenario.add_real_card(P0, CARD, Zone::Hand, db);
    let murder = scenario.add_real_card(P0, "Murder", Zone::Hand, db);
    let mut mana = red_mana(3);
    mana.extend([
        ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    ]);
    scenario.with_mana_pool(P0, mana);

    let mut runner = scenario.build();
    let mut goblin_cast = runner.cast(entering).commit();
    let state = goblin_cast.state_mut();
    state.rng_seed = 0;
    state.rng_word_pos = 0;
    state.rng = ChaCha20Rng::seed_from_u64(0);

    // Resolve the creature spell, but stop at the ordinary priority window with
    // its ETB trigger on the stack. This reach guard ensures the later no-mana
    // result comes from the intervening-if recheck, not a trigger that never
    // fired in the first place.
    for _ in 0..8 {
        if goblin_cast.state().objects[&entering].zone == Zone::Battlefield
            && goblin_cast.state().stack.iter().any(|entry| {
                entry.source_id == entering
                    && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
            })
        {
            break;
        }
        goblin_cast
            .act(engine::types::actions::GameAction::PassPriority)
            .expect("cast pipeline must advance to the ETB trigger");
    }
    assert_eq!(
        goblin_cast.state().objects[&entering].zone,
        Zone::Battlefield
    );
    assert!(
        goblin_cast.state().stack.iter().any(|entry| {
            entry.source_id == entering
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        }),
        "the Name Sticker Goblin ETB must be on the stack before Murder resolves"
    );

    // Use a real response spell through the cast pipeline rather than mutating
    // zones: Murder removes the source while its already-triggered ETB waits.
    let mut murder_cast = goblin_cast
        .cast(murder)
        .target_objects(&[entering])
        .commit();
    for _ in 0..8 {
        if murder_cast.state().objects[&entering].zone == Zone::Graveyard {
            break;
        }
        murder_cast
            .act(engine::types::actions::GameAction::PassPriority)
            .expect("Murder must resolve before the waiting ETB trigger");
    }
    assert_eq!(murder_cast.state().objects[&entering].zone, Zone::Graveyard);
    assert!(
        murder_cast.state().stack.iter().any(|entry| {
            entry.source_id == entering
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        }),
        "the waiting ETB trigger must survive long enough to recheck its condition"
    );

    let mut events = Vec::new();
    for _ in 0..8 {
        if murder_cast.state().stack.is_empty() {
            break;
        }
        events.extend(
            murder_cast
                .act(engine::types::actions::GameAction::PassPriority)
                .expect("the waiting ETB trigger must resolve cleanly")
                .events,
        );
    }
    assert!(
        murder_cast.state().stack.is_empty(),
        "ETB trigger must settle"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::DieRolled { sides: 20, .. })),
        "CR 603.4 recheck must prevent the roll once the source has left the battlefield"
    );
    assert_eq!(
        mana_red_count(murder_cast.state()),
        0,
        "a false battlefield intervening-if must add no red mana"
    );
}

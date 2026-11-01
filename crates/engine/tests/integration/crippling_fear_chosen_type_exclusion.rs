//! Crippling Fear {2}{B}{B} Sorcery — verbatim Oracle text (Scryfall):
//! "Choose a creature type. Creatures that aren't of the chosen type get -3/-3
//! until end of turn."
//!
//! CR 205.3m + CR 611.2c: the plural subject "creatures that aren't of the chosen
//! type" is a relative clause over the whole battlefield, so the spell is a mass
//! pump over every creature outside the chosen creature type, both players'
//! creatures, with no target; the set it affects is fixed as the spell resolves.
//! The relative-clause copula "aren't" used to be mistaken for the sentence
//! predicate, truncating the subject at "creatures that" and lowering the pump to
//! a single-target `Pump` at `Any`.
//!
//! Every negative assertion (the chosen type is untouched) is paired with a
//! positive reach-guard in the same test (a creature outside the type shrank).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CRIPPLING_FEAR: &str =
    "Choose a creature type. Creatures that aren't of the chosen type get -3/-3 until end of turn.";

struct Board {
    runner: GameRunner,
    own_elf: ObjectId,
    own_human: ObjectId,
    opposing_elf: ObjectId,
    opposing_goblin: ObjectId,
}

/// Cast Crippling Fear with a real `{2}{B}{B}` payment and answer the creature
/// type choice with `chosen`. The test never declares a target: the spell has none.
fn resolve_crippling_fear(chosen: &str) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let own_elf = scenario
        .add_creature(P0, "Own Elf", 3, 3)
        .with_subtypes(vec!["Elf"])
        .id();
    let own_human = scenario
        .add_creature(P0, "Own Human", 3, 3)
        .with_subtypes(vec!["Human"])
        .id();
    let opposing_elf = scenario
        .add_creature(P1, "Opposing Elf", 3, 3)
        .with_subtypes(vec!["Elf"])
        .id();
    let opposing_goblin = scenario
        .add_creature(P1, "Opposing Goblin", 4, 4)
        .with_subtypes(vec!["Goblin"])
        .id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Crippling Fear", false, CRIPPLING_FEAR)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 2,
        })
        .id();

    let mut runner = scenario.build();
    // `GameScenario` leaves the creature-type list empty; the chooser offers it.
    runner.state_mut().all_creature_types = ["Elf", "Human", "Goblin"]
        .iter()
        .map(|t| t.to_string())
        .collect();
    for _ in 0..4 {
        runner.state_mut().players[P0.0 as usize]
            .mana_pool
            .add(ManaUnit::new(ManaType::Black, spell, false, vec![]));
    }

    // No `.target_*` intent: a `TargetSelection` prompt here means the spell
    // regressed to a targeted pump and the driver panics.
    runner.cast(spell).choose_option(chosen).resolve();

    Board {
        runner,
        own_elf,
        own_human,
        opposing_elf,
        opposing_goblin,
    }
}

fn pt(board: &Board, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let obj = &board.runner.state().objects[&id];
    (obj.power, obj.toughness)
}

fn zone(board: &Board, id: ObjectId) -> Zone {
    board.runner.state().objects[&id].zone
}

#[test]
fn creatures_outside_the_chosen_type_get_minus_three_on_both_sides() {
    let board = resolve_crippling_fear("Elf");

    // Positive reach-guard: the pump applied to a creature outside the chosen type
    // on the OPPOSING side (the spell has no controller restriction)…
    assert_eq!(
        pt(&board, board.opposing_goblin),
        (Some(1), Some(1)),
        "4/4 Goblin is not an Elf: -3/-3 leaves a 1/1"
    );
    assert_eq!(zone(&board, board.opposing_goblin), Zone::Battlefield);

    // …and on the caster's own side: a 3/3 Human takes -3/-3 and dies (CR 704.5f).
    assert_eq!(
        zone(&board, board.own_human),
        Zone::Graveyard,
        "3/3 Human is not an Elf: toughness 0 is put into the graveyard"
    );

    // Negative: creatures of the chosen type are untouched, on both sides.
    assert_eq!(pt(&board, board.own_elf), (Some(3), Some(3)));
    assert_eq!(pt(&board, board.opposing_elf), (Some(3), Some(3)));
    assert_eq!(zone(&board, board.own_elf), Zone::Battlefield);
    assert_eq!(zone(&board, board.opposing_elf), Zone::Battlefield);
}

#[test]
fn the_exclusion_follows_the_chosen_type_not_a_fixed_one() {
    // Same board, different answer: now the Goblin is the protected type and the
    // Elves/Human are outside it. The 3/3 Elves die; the Goblin is untouched.
    let board = resolve_crippling_fear("Goblin");

    assert_eq!(zone(&board, board.own_elf), Zone::Graveyard);
    assert_eq!(zone(&board, board.opposing_elf), Zone::Graveyard);
    assert_eq!(zone(&board, board.own_human), Zone::Graveyard);
    assert_eq!(
        pt(&board, board.opposing_goblin),
        (Some(4), Some(4)),
        "the chosen type (Goblin) is not affected"
    );
    assert_eq!(zone(&board, board.opposing_goblin), Zone::Battlefield);
}

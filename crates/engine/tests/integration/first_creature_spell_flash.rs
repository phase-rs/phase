//! CR 601.3b + CR 702.8a + CR 302.1: Radagast of Rhosgobel ("The first creature spell you cast
//! each turn costs {2} less to cast and can be cast as though it had flash.") and The Blue Spirit
//! ("You may cast the first creature spell you cast each turn as though it had flash.") let only
//! the first creature spell each turn be cast at instant speed. Both sources are built from
//! their Oracle text so the permission comes from the parser at test time.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const RADAGAST_OF_RHOSGOBEL: &str = "The first creature spell you cast each turn costs {2} less \
to cast and can be cast as though it had flash.";

const THE_BLUE_SPIRIT: &str = "You may cast the first creature spell you cast each turn as \
though it had flash.\nWhenever a nontoken creature you control enters during combat, draw a card.";

const OPT: &str = "Scry 1. (Look at the top card of your library. You may put that card on the \
bottom.)\nDraw a card.";

struct Board {
    runner: GameRunner,
    bears: [ObjectId; 2],
    opt: ObjectId,
}

/// `source` (2/`toughness`), two Grizzly Bears and Opt for `owner`, with six green and one blue
/// mana floating, during P0's turn at `phase`.
fn board(phase: Phase, owner: PlayerId, source: (&str, i32, &str)) -> Board {
    let db = load_db().expect("the committed integration card fixture loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    let (name, toughness, oracle) = source;
    scenario.add_creature_from_oracle(owner, name, 2, toughness, oracle);
    let bears = [
        scenario.add_real_card(owner, "Grizzly Bears", Zone::Hand, db),
        scenario.add_real_card(owner, "Grizzly Bears", Zone::Hand, db),
    ];
    let opt = scenario
        .add_spell_to_hand_from_oracle(owner, "Opt", true, OPT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    scenario.with_library_top(owner, &["Library A", "Library B"]);
    let mut mana = vec![ManaUnit::new(ManaType::Green, ObjectId(9_900), false, vec![]); 6];
    mana.push(ManaUnit::new(
        ManaType::Blue,
        ObjectId(9_901),
        false,
        vec![],
    ));
    scenario.with_mana_pool(owner, mana);
    Board {
        runner: scenario.build(),
        bears,
        opt,
    }
}

fn radagast(phase: Phase, owner: PlayerId) -> Board {
    board(
        phase,
        owner,
        ("Radagast of Rhosgobel", 5, RADAGAST_OF_RHOSGOBEL),
    )
}

fn blue_spirit(phase: Phase) -> Board {
    board(phase, P0, ("The Blue Spirit", 4, THE_BLUE_SPIRIT))
}

fn cast(runner: &mut GameRunner, spell: ObjectId) -> bool {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: Vec::new(),
            payment_mode: Default::default(),
        })
        .is_ok()
}

fn pool(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].mana_pool.total()
}

fn on_stack(runner: &GameRunner, spell: ObjectId) -> bool {
    runner.state().stack.iter().any(|entry| entry.id == spell)
}

/// Casts both Bears in P0's upkeep, asserting the first is legal at instant speed with the
/// expected mana spent and the second is refused.
fn assert_first_creature_only_in_upkeep(mut b: Board, first_cost: usize) {
    let before = pool(&b.runner, P0);
    assert!(
        cast(&mut b.runner, b.bears[0]),
        "first creature spell has flash"
    );
    assert!(on_stack(&b.runner, b.bears[0]));
    assert_eq!(pool(&b.runner, P0), before - first_cost);
    b.runner.advance_until_stack_empty();
    assert_eq!(b.runner.state().phase, Phase::Upkeep);
    assert!(
        !cast(&mut b.runner, b.bears[1]),
        "second creature spell this turn needs sorcery timing"
    );
}

/// Reach guard: the same board at sorcery timing casts both Bears, so an upkeep refusal is the
/// timing rule rather than mana or permission.
fn assert_both_castable_in_main(mut b: Board) {
    assert!(cast(&mut b.runner, b.bears[0]));
    b.runner.advance_until_stack_empty();
    assert!(cast(&mut b.runner, b.bears[1]));
}

#[test]
fn radagast_first_creature_spell_has_flash_second_does_not() {
    assert_both_castable_in_main(radagast(Phase::PreCombatMain, P0));
    // CR 601.2f: Grizzly Bears' {1}{G} less {2} costs only {G}.
    assert_first_creature_only_in_upkeep(radagast(Phase::Upkeep, P0), 1);
}

#[test]
fn the_blue_spirit_first_creature_spell_has_flash_second_does_not() {
    assert_both_castable_in_main(blue_spirit(Phase::PreCombatMain));
    assert_first_creature_only_in_upkeep(blue_spirit(Phase::Upkeep), 2);
}

#[test]
fn radagast_noncreature_spell_does_not_spend_the_first_creature_window() {
    let Board {
        mut runner,
        bears,
        opt,
    } = radagast(Phase::Upkeep, P0);
    runner
        .cast(opt)
        .try_resolve()
        .expect("Opt is an instant, castable in the upkeep");
    assert!(!on_stack(&runner, opt));
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert!(
        cast(&mut runner, bears[0]),
        "Opt is not a creature spell, so the first creature spell still has flash"
    );
}

#[test]
fn radagast_grants_flash_on_an_opponents_turn() {
    let Board {
        mut runner, bears, ..
    } = radagast(Phase::Upkeep, P1);
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority in its upkeep");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "P1 holds priority during P0's upkeep: {:?}",
        runner.state().waiting_for
    );
    assert!(
        cast(&mut runner, bears[0]),
        "first creature spell P1 casts during P0's turn has flash"
    );
    assert!(on_stack(&runner, bears[0]));
}

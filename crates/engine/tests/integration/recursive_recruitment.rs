//! Recursive Recruitment (Reality Fracture) and the "each of them" class.
//!
//! "Create two 2/2 colorless Wizard Soldier creature tokens named Cadet. If
//! this spell was cast from a graveyard, put a +1/+1 counter on each of them
//! for every three cards in your graveyard."
//!
//! - CR 608.2c + CR 111.1: "each of them" after a token clause is the tokens
//!   that clause just created (`TargetFilter::LastCreated`).
//! - CR 107.1a: "for every three cards" counts complete groups (floor).
//! - CR 601.2a + CR 702.34a: a flashback spell is on the stack while it
//!   resolves (so it does not count itself) and is exiled as it leaves.
//! - CR 404.1: only "your graveyard" counts.
//!
//! Class rows: Snarl Song and Assemble the Entmoot use the same "on each of
//! them" anaphor after a token clause; before the fix the counters went
//! nowhere.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::counter::{parse_counter_type, CounterType};
use engine::types::game_state::{CastingVariant, GameState};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;
use engine::types::PlayerId;

const RECURSIVE_RECRUITMENT: &str = "Create two 2/2 colorless Wizard Soldier creature tokens named Cadet. If this spell was cast from a graveyard, put a +1/+1 counter on each of them for every three cards in your graveyard.\nFlashback {6}{U}{B}";

const SNARL_SONG: &str = "Converge — Create two 0/0 green and blue Fractal creature tokens. Put X +1/+1 counters on each of them and you gain X life, where X is the number of colors of mana spent to cast this spell.";

const ASSEMBLE_THE_ENTMOOT: &str = "Creatures you control have reach.\nSacrifice this enchantment: Create three tapped X/X green Treefolk creature tokens, where X is the amount of life you gained this turn. Put a reach counter on each of them.";

fn fund(runner: &mut GameRunner, player: PlayerId, mana: &[ManaType]) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, ObjectId(0), false, vec![]));
    }
}

/// {6}{U}{B}
const FLASHBACK_MANA: [ManaType; 8] = [
    ManaType::Blue,
    ManaType::Black,
    ManaType::Colorless,
    ManaType::Colorless,
    ManaType::Colorless,
    ManaType::Colorless,
    ManaType::Colorless,
    ManaType::Colorless,
];

/// +1/+1 counters on each battlefield token named `name` controlled by P0.
fn token_counters(state: &GameState, name: &str) -> Vec<u32> {
    state
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield && o.is_token && o.controller == P0 && o.name == name
        })
        .map(|o| {
            o.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        })
        .collect()
}

fn stock_graveyard(scenario: &mut GameScenario, player: PlayerId, count: usize, tag: &str) {
    for i in 0..count {
        scenario.add_creature_to_graveyard(player, &format!("{tag} Filler {i}"), 1, 1);
    }
}

/// Flashback-cast Recursive Recruitment with `mine` OTHER cards in P0's
/// graveyard and `theirs` in P1's. Returns the Cadet counters and the spell's
/// final zone.
fn flashback_with(mine: usize, theirs: usize) -> (Vec<u32>, Option<Zone>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut b = scenario.add_spell_to_graveyard(P0, "Recursive Recruitment", false);
    b.from_oracle_text(RECURSIVE_RECRUITMENT);
    let spell = b.id();
    stock_graveyard(&mut scenario, P0, mine, "Mine");
    stock_graveyard(&mut scenario, P1, theirs, "Theirs");
    let mut runner = scenario.build();
    fund(&mut runner, P0, &FLASHBACK_MANA);

    let out = runner
        .cast(spell)
        .casting_variant(CastingVariant::Flashback)
        .resolve();
    let zone = out.state().objects.get(&spell).map(|o| o.zone);
    (token_counters(out.state(), "Cadet"), zone)
}

/// Row 6: floor(other graveyard cards / 3) counters on EACH Cadet. The spell
/// is on the stack during resolution, so it never counts itself: 2 other
/// cards gives [0, 0] (counting the spell would give [1, 1]) and 5 gives
/// [1, 1] (counting it would give [2, 2]).
///
/// REVERT-PROBE: before the fix the clause was `Effect::Unimplemented`, so
/// every row had [0, 0]; the 3/6/9 rows fail.
#[test]
fn recursive_recruitment_flashback_counts_complete_groups_of_three() {
    for (others, expected) in [
        (0, vec![0, 0]),
        (2, vec![0, 0]),
        (3, vec![1, 1]),
        (5, vec![1, 1]),
        (6, vec![2, 2]),
        (9, vec![3, 3]),
    ] {
        let (counters, zone) = flashback_with(others, 0);
        assert_eq!(
            counters.len(),
            2,
            "reach-guard ({others} others): exactly two Cadet tokens, got {counters:?}"
        );
        assert_eq!(
            zone,
            Some(Zone::Exile),
            "reach-guard ({others} others): flashback spell is exiled (CR 702.34a)"
        );
        assert_eq!(
            counters, expected,
            "{others} other graveyard cards → floor({others} / 3) counters on each Cadet"
        );
    }
}

/// Row 7: a hard cast from hand fails `WasCast { Graveyard }`, so no counters
/// even with 9 graveyard cards. The paired positive (same 9 cards, flashback
/// → [3, 3]) is in the same test so the negative is not vacuous.
#[test]
fn recursive_recruitment_hard_cast_puts_no_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut b = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Recursive Recruitment",
        false,
        RECURSIVE_RECRUITMENT,
    );
    b.with_mana_cost(ManaCost::generic(0));
    let spell = b.id();
    stock_graveyard(&mut scenario, P0, 9, "Mine");
    let mut runner = scenario.build();

    let out = runner.cast(spell).resolve();
    let counters = token_counters(out.state(), "Cadet");
    assert_eq!(
        counters.len(),
        2,
        "reach-guard: two Cadets created by the hard cast, got {counters:?}"
    );
    assert_eq!(counters, vec![0, 0], "hard cast → no counters");

    let (flashback, _) = flashback_with(9, 0);
    assert_eq!(
        flashback,
        vec![3, 3],
        "paired positive: same 9 cards via flashback → three on each"
    );
}

/// Row 8: only the controller's graveyard counts (CR 404.1). Six cards in the
/// opponent's graveyard plus two in ours → [0, 0]; three in ours → [1, 1].
#[test]
fn recursive_recruitment_counts_only_your_graveyard() {
    let (counters, _) = flashback_with(2, 6);
    assert_eq!(
        counters.len(),
        2,
        "reach-guard: two Cadets, got {counters:?}"
    );
    assert_eq!(
        counters,
        vec![0, 0],
        "opponent's graveyard cards must not count"
    );

    let (counters, _) = flashback_with(3, 6);
    assert_eq!(
        counters,
        vec![1, 1],
        "paired positive: three cards in your graveyard → one on each"
    );
}

/// Row 9: Snarl Song. Two colors spent → X = 2: both 0/0 Fractals get two
/// +1/+1 counters and survive, and the controller gains 2 life.
///
/// REVERT-PROBE: before the fix "each of them" was an unbound `ParentTarget`,
/// so the Fractals stayed 0/0 and died to state-based actions (no tokens left).
#[test]
fn snarl_song_counters_land_on_each_created_fractal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut b = scenario.add_spell_to_hand_from_oracle(P0, "Snarl Song", false, SNARL_SONG);
    b.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
        generic: 0,
    });
    let spell = b.id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;
    fund(&mut runner, P0, &[ManaType::Green, ManaType::Blue]);

    let out = runner.cast(spell).resolve();
    let life_after = out.state().players[P0.0 as usize].life;
    assert_eq!(
        life_after - life_before,
        2,
        "reach-guard: the compound life-gain tail ran with X = 2"
    );
    assert_eq!(
        token_counters(out.state(), "Fractal"),
        vec![2, 2],
        "each Fractal gets X = 2 counters and survives"
    );
}

/// Row 10: Assemble the Entmoot. After gaining 3 life this turn, sacrificing
/// it creates three tapped 3/3 Treefolk, each with a reach counter.
///
/// REVERT-PROBE: before the fix the reach counter went to an unbound
/// `ParentTarget` and no Treefolk had one.
#[test]
fn assemble_the_entmoot_reach_counter_on_each_treefolk() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let entmoot = scenario
        .add_enchantment_from_oracle(P0, "Assemble the Entmoot", ASSEMBLE_THE_ENTMOOT)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].life_gained_this_turn = 3;

    let out = runner.activate(entmoot, 0).resolve();
    let reach = parse_counter_type("reach");
    let treefolk: Vec<_> = out
        .state()
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.is_token
                && o.controller == P0
                && o.card_types.subtypes.iter().any(|s| s == "Treefolk")
        })
        .collect();
    assert_eq!(
        treefolk.len(),
        3,
        "reach-guard: the token clause made exactly three Treefolk"
    );
    for t in &treefolk {
        assert!(t.tapped, "each Treefolk enters tapped");
        assert_eq!(t.power, Some(3), "X/X sized from 3 life gained");
        assert_eq!(t.toughness, Some(3), "X/X sized from 3 life gained");
        assert_eq!(
            t.counters.get(&reach).copied().unwrap_or(0),
            1,
            "each Treefolk gets a reach counter"
        );
    }
}

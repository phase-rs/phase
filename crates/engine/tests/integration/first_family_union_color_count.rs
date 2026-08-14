//! First Family — "the number of colors among permanents you control **and**
//! spells you've cast this turn" is a SET UNION over two populations, resolved
//! through the real cast pipeline.
//!
//! Pre-fix, both quantity slots bound `QuantityRef::SpellsCastThisTurn` — a
//! COUNT OF SPELLS — silently dropping both the colour aggregation (CR 105.1)
//! and the "permanents you control" population, with no parse warning. Every
//! assertion below flips if that binding is restored:
//!
//!   * the self-inclusion case returns 1 (one cast record) instead of 2;
//!   * the disjoint case does not move when a permanent of a NEW colour is
//!     added, because the permanent population is not read at all;
//!   * a `Sum`-shaped implementation (`|A| + |B|`) over-counts the overlap,
//!     which is why the union must live inside the `HashSet` — `max`
//!     distributes over set union but a distinct-count does not.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Verbatim Scryfall Oracle text — a paraphrase can take a different parser
/// branch and go green while the real card stays broken.
const FIRST_FAMILY: &str = "You draw X cards and gain X life, where X is the number of colors \
among permanents you control and spells you've cast this turn.";

/// A vanilla filler instant used only to put a cast record of a known colour
/// into the per-turn journal (CR 601.2a: casting moves the card to the stack,
/// which is where `finalize_cast` records it).
const FILLER: &str = "Target player gains 1 life.";

fn mono(shard: ManaCostShard) -> ManaCost {
    ManaCost::Cost {
        shards: vec![shard],
        generic: 0,
    }
}

/// First Family's printed cost: {2}{G}{U}. CR 105.2 — the spell is green AND
/// blue, so its own cast record contributes two colours.
fn first_family_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Green, ManaCostShard::Blue],
        generic: 2,
    }
}

fn pool(colored: &[(ManaType, usize)]) -> Vec<ManaUnit> {
    colored
        .iter()
        .flat_map(|(kind, n)| vec![ManaUnit::new(*kind, ObjectId(0), false, vec![]); *n])
        .collect()
}

/// Enough mana for the filler plus First Family, with the exact colours both
/// need. Colour identity of the SPELL comes from its printed cost, not from the
/// mana spent (CR 105.2), so the pool composition is payment only.
fn full_pool() -> Vec<ManaUnit> {
    pool(&[
        (ManaType::Green, 3),
        (ManaType::Blue, 1),
        (ManaType::Colorless, 2),
    ])
}

/// CR 117.1: hand priority to `player` so their cast runs through the SAME
/// production `GameAction::CastSpell` path every other cast in this file uses,
/// rather than being injected into the journal by hand.
fn give_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

/// CR 105.1 + CR 112.1 + CR 601.2a. The full multi-authority overlap fixture:
/// a green permanent, a green spell cast EARLIER this turn, and First Family
/// itself ({G}{U}).
///
///   colours = { G (permanent), G (earlier spell), G+U (First Family) } = {G, U}
///
/// So X = 2 — NOT 4, which is what a `Sum` of the two populations' independent
/// colour counts (|{G}| + |{G,U}|) would give. The overlap is the whole point:
/// green is in BOTH populations and must contribute once.
#[test]
fn first_family_counts_the_union_not_the_sum_on_overlap() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C", "Draw D"]);

    // Population A: one GREEN permanent.
    scenario
        .add_creature(P0, "Green Bear", 2, 2)
        .with_mana_cost(mono(ManaCostShard::Green));

    let filler = scenario
        .add_spell_to_hand_from_oracle(P0, "Green Filler", true, FILLER)
        .with_mana_cost(mono(ManaCostShard::Green))
        .id();
    let first_family = scenario
        .add_spell_to_hand_from_oracle(P0, "First Family", true, FIRST_FAMILY)
        .with_mana_cost(first_family_cost())
        .id();

    scenario.with_mana_pool(P0, full_pool());
    let mut runner = scenario.build();

    // Population B, member 1: a GREEN spell cast earlier this turn.
    runner.cast(filler).target_player(P0).resolve();
    // Reach guard: the earlier cast really is journaled, so the union below is
    // exercised over TWO non-empty populations rather than degenerating to one.
    assert_eq!(
        runner
            .state()
            .spells_cast_this_turn_by_player
            .get(&P0)
            .map_or(0, |records| records.len()),
        1,
        "reach guard: the earlier green cast must be in the journal"
    );

    give_priority(&mut runner, P0);
    let outcome = runner.cast(first_family).resolve();

    outcome.assert_hand_drawn(P0, 2);
    // The filler gained 1 life before First Family resolved; `life_delta` is
    // measured from just before THIS cast, so it isolates First Family's gain.
    outcome.assert_life_delta(P0, 2);
}

/// CR 105.1. Adding a permanent of a colour that is in NEITHER population moves
/// X from 2 to 3. This is the assertion a spell-COUNT implementation cannot
/// pass: the number of spells cast is unchanged by a permanent being on the
/// battlefield.
#[test]
fn first_family_reads_the_permanent_population() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C", "Draw D"]);

    scenario
        .add_creature(P0, "Green Bear", 2, 2)
        .with_mana_cost(mono(ManaCostShard::Green));
    // The disjoint third colour — present only on the battlefield.
    scenario
        .add_creature(P0, "Red Bear", 2, 2)
        .with_mana_cost(mono(ManaCostShard::Red));

    let filler = scenario
        .add_spell_to_hand_from_oracle(P0, "Green Filler", true, FILLER)
        .with_mana_cost(mono(ManaCostShard::Green))
        .id();
    let first_family = scenario
        .add_spell_to_hand_from_oracle(P0, "First Family", true, FIRST_FAMILY)
        .with_mana_cost(first_family_cost())
        .id();

    scenario.with_mana_pool(P0, full_pool());
    let mut runner = scenario.build();

    runner.cast(filler).target_player(P0).resolve();
    give_priority(&mut runner, P0);
    let outcome = runner.cast(first_family).resolve();

    // {G (bear + filler + FF), R (bear), U (FF)} = 3. The pre-fix spell-count
    // reading is 2 here (two cast records), so this row discriminates.
    outcome.assert_hand_drawn(P0, 3);
    outcome.assert_life_delta(P0, 3);
}

/// CR 112.1 + CR 608.2m: First Family counts ITS OWN cast. Its record is pushed
/// at `finalize_cast` and, per CR 112.1, it is still on the stack while its own
/// effect resolves — so with an empty board and no prior spells X is 2 (green
/// and blue), never 0 or 1.
///
/// This also pins the ABSENCE of the `FilterProp::Another` own-cast exclusion:
/// the card says "spells you've cast", not "OTHER spells you've cast". Applying
/// that exclusion here would give 0.
#[test]
fn first_family_counts_its_own_cast_on_an_empty_board() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C"]);

    let first_family = scenario
        .add_spell_to_hand_from_oracle(P0, "First Family", true, FIRST_FAMILY)
        .with_mana_cost(first_family_cost())
        .id();

    scenario.with_mana_pool(P0, full_pool());
    let mut runner = scenario.build();

    let outcome = runner.cast(first_family).resolve();

    // REVERT GUARD, the sharpest one: the pre-fix spell-count reading is 1 here
    // (exactly one cast record — First Family's own).
    outcome.assert_hand_drawn(P0, 2);
    outcome.assert_life_delta(P0, 2);
}

/// CR 109.4: "spells **you've** cast" is the controller's journal
/// (`CountScope::Controller`). An opponent's cast of a brand-new colour must not
/// move X.
#[test]
fn first_family_ignores_an_opponents_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw A", "Draw B", "Draw C"]);

    scenario
        .add_creature(P0, "Green Bear", 2, 2)
        .with_mana_cost(mono(ManaCostShard::Green));

    let opponent_spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Black Filler", true, FILLER)
        .with_mana_cost(mono(ManaCostShard::Black))
        .id();
    let first_family = scenario
        .add_spell_to_hand_from_oracle(P0, "First Family", true, FIRST_FAMILY)
        .with_mana_cost(first_family_cost())
        .id();

    scenario.with_mana_pool(P0, full_pool());
    scenario.with_mana_pool(P1, pool(&[(ManaType::Black, 1)]));
    let mut runner = scenario.build();

    give_priority(&mut runner, P1);
    runner.cast(opponent_spell).target_player(P1).resolve();

    // Reach guard: the opponent's BLACK cast really did happen and really is
    // journaled, so the negative below is not vacuous — the record exists and
    // is simply out of scope.
    assert_eq!(
        runner
            .state()
            .spells_cast_this_turn_by_player
            .get(&P1)
            .map_or(0, |records| records.len()),
        1,
        "reach guard: the opponent's cast must be journaled before we assert it is ignored"
    );

    give_priority(&mut runner, P0);
    let outcome = runner.cast(first_family).resolve();

    // {G (bear + FF), U (FF)} = 2. Black is in the OPPONENT's journal only; a
    // `CountScope::All` reading would give 3.
    outcome.assert_hand_drawn(P0, 2);
    outcome.assert_life_delta(P0, 2);
}

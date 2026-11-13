//! CR 109.2 + CR 109.2a + CR 601.2c + CR 115.9: the "targets a <type>"
//! qualifier on a cost modifier means a permanent of that type ON THE
//! BATTLEFIELD, so a spell that targeted a creature CARD IN A GRAVEYARD must
//! not satisfy it.
//!
//! Head of the Class (Scryfall `ysos/21`, verified verbatim):
//!   "The first spell you cast during each of your turns that targets a
//!    creature costs {W}{B} less to cast. This effect reduces only the amount of
//!    colored mana you pay."
//!
//! Reanimate (Scryfall `dsc/155`, verified verbatim):
//!   "Put target creature card from a graveyard onto the battlefield under your
//!    control. You lose life equal to that card's mana value."
//!
//! CR 109.2 scopes the first sentence's bare "a creature" to the battlefield:
//! "If a spell or ability uses a description of an object that includes a card
//! type or subtype, but doesn't refer to a specific zone or include the word
//! 'card', 'spell', 'source', or 'scheme', it means a permanent of that card
//! type or subtype on the battlefield." Reanimate's qualifier is the CR 109.2a
//! shape instead — it says "card" and names a zone — so its target is a card in
//! a graveyard, never a battlefield permanent.
//!
//! The defect this file pins: the cast-history target fact carried no zone, so
//! the record's target was evaluated as though it sat on the battlefield. A
//! resolved Reanimate therefore counted as "the first spell you cast this turn
//! that targets a creature" and consumed Head of the Class's slot for a
//! battlefield-creature spell cast afterwards.
//!
//! Legs:
//!   * witness — cast Reanimate at a creature card in a graveyard, resolve it,
//!     then cast a battlefield-creature spell: it still pays the discounted {1}.
//!     Pre-fix it paid the full {1}{B} (2).
//!   * witness (same fact, other side) — Reanimate does not claim the discount
//!     itself, because it is not "a spell that targets a creature". Pre-fix it
//!     cost {0} instead of its printed {B}.
//!   * control — a spell targeting a BATTLEFIELD creature really does consume
//!     the slot, so the next one pays full price. Without this leg the witnesses
//!     would pass vacuously if the qualifier matched nothing at all.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Head of the Class — complete printed Oracle text, both abilities. The first
/// sentence is the CR 601.2f reducer under test; the second is the printed
/// Repartee trigger, which the card really has and which the fixture must carry
/// so the whole parse → layers → cost pipeline sees the real card.
const HEAD_OF_THE_CLASS_ORACLE: &str = "The first spell you cast during each of your turns that targets a creature costs {W}{B} less to cast. This effect reduces only the amount of colored mana you pay.\nRepartee — Whenever you cast an instant or sorcery spell that targets a creature, this creature perpetually gets +1/+1.";

/// Reanimate — complete printed Oracle text, verbatim from Scryfall `dsc/155`.
/// The target qualifier is the load-bearing part: "creature card from a
/// graveyard", not a battlefield creature.
const REANIMATE_ORACLE: &str = "Put target creature card from a graveyard onto the battlefield under your control. You lose life equal to that card's mana value.";

/// Doom Blade's printed `{1}{B}`: full cost spends 2, Head of the Class's
/// `{W}{B}` reduction spends 1. The `{W}` half is lost because the card's own
/// rider restricts the reduction to colored mana actually paid — the default
/// spillover into generic would be CR 118.7b — and the `{B}` half removes the
/// black pip. The one-mana delta is what makes the discount observable.
fn doom_blade_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black],
        generic: 1,
    }
}

/// Reanimate's printed `{B}`.
fn reanimate_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black],
        generic: 0,
    }
}

/// Head of the Class + two battlefield victims (the first is destroyed by any
/// prior battlefield-creature cast, so a second one still has a legal target) +
/// two Doom Blades + Reanimate + one creature card in the graveyard.
struct ZoneBoard {
    runner: GameRunner,
    blade_a: ObjectId,
    blade_b: ObjectId,
    victim_a: ObjectId,
    victim_b: ObjectId,
    reanimate: ObjectId,
    graveyard_creature: ObjectId,
}

fn zone_board() -> ZoneBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Head of the Class", 2, 2, HEAD_OF_THE_CLASS_ORACLE);
    let victim_a = scenario.add_vanilla(P0, 1, 1);
    let victim_b = scenario.add_vanilla(P0, 1, 1);
    let blade_a = scenario
        .add_spell_to_hand_from_oracle(P0, "Doom Blade", true, "Destroy target nonblack creature.")
        .with_mana_cost(doom_blade_cost())
        .id();
    let blade_b = scenario
        .add_spell_to_hand_from_oracle(P0, "Doom Blade", true, "Destroy target nonblack creature.")
        .with_mana_cost(doom_blade_cost())
        .id();
    let reanimate = scenario
        .add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE_ORACLE)
        .with_mana_cost(reanimate_cost())
        .id();
    let graveyard_creature = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    ZoneBoard {
        runner: scenario.build(),
        blade_a,
        blade_b,
        victim_a,
        victim_b,
        reanimate,
        graveyard_creature,
    }
}

/// Fill P0's pool with `black` black and `colorless` colorless mana.
fn fill_pool(runner: &mut GameRunner, black: usize, colorless: usize) {
    for _ in 0..black {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Black,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    for _ in 0..colorless {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

/// Cast `spell` (targeting `victim`) and report the mana spent.
fn cast_and_measure(runner: &mut GameRunner, spell: ObjectId, victim: ObjectId) -> usize {
    let before = runner.state().players[0].mana_pool.total();
    runner.cast(spell).target_objects(&[victim]).resolve();
    before - runner.state().players[0].mana_pool.total()
}

/// CR 109.2 + CR 601.2c: Reanimate's target is a creature CARD IN A GRAVEYARD,
/// so the resolved Reanimate is not "a spell that targets a creature" and must
/// not consume Head of the Class's once-per-turn slot.
#[test]
fn reanimate_does_not_consume_the_first_creature_targeting_spell_slot() {
    let mut board = zone_board();
    fill_pool(&mut board.runner, 4, 4);

    cast_and_measure(&mut board.runner, board.reanimate, board.graveyard_creature);
    assert!(
        board
            .runner
            .state()
            .objects
            .get(&board.graveyard_creature)
            .map(|object| object.zone)
            == Some(Zone::Battlefield),
        "reach guard: Reanimate resolved and put the creature card onto the battlefield"
    );

    // The later spell targets a BATTLEFIELD creature — CR 109.2's bare "a
    // creature" — so it is the first qualifying spell of the turn.
    let spent_blade = cast_and_measure(&mut board.runner, board.blade_a, board.victim_a);
    assert_eq!(
        spent_blade, 1,
        "CR 109.2: Reanimate targeted a creature CARD IN A GRAVEYARD, so it must not \
         have consumed Head of the Class's slot; the later battlefield-creature spell \
         still pays the discounted {{1}} (pre-fix: it paid the full {{1}}{{B}})"
    );
}

/// The same announcement-zone fact, read from the other side: because Reanimate
/// targeted a creature CARD IN A GRAVEYARD, Head of the Class must not discount
/// Reanimate itself. Pre-fix it did, and the printed `{B}` cost `{0}`.
#[test]
fn reanimate_does_not_take_the_discount_itself() {
    let mut board = zone_board();
    fill_pool(&mut board.runner, 4, 4);

    let spent_reanimate =
        cast_and_measure(&mut board.runner, board.reanimate, board.graveyard_creature);
    assert_eq!(
        spent_reanimate, 1,
        "CR 109.2: Reanimate targets a creature card in a graveyard, so it is not a \
         spell that targets a creature and pays its own printed {{B}} (pre-fix: {{0}})"
    );
    assert!(
        board
            .runner
            .state()
            .objects
            .get(&board.graveyard_creature)
            .map(|object| object.zone)
            == Some(Zone::Battlefield),
        "reach guard: Reanimate resolved and put the creature card onto the battlefield"
    );
}

/// Paired control: a spell that targets a BATTLEFIELD creature really does
/// consume the slot, so the witness above is not vacuous.
#[test]
fn a_battlefield_creature_target_does_consume_the_slot() {
    let mut board = zone_board();
    fill_pool(&mut board.runner, 4, 4);

    let first = cast_and_measure(&mut board.runner, board.blade_a, board.victim_a);
    assert_eq!(
        first, 1,
        "the first battlefield-creature-targeting spell gets the discount"
    );

    let second = cast_and_measure(&mut board.runner, board.blade_b, board.victim_b);
    assert_eq!(
        second, 2,
        "control: a spell that targeted a battlefield creature DID consume the slot, \
         so the second pays full {{1}}{{B}}"
    );
}

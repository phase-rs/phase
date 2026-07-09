//! CR 603.12 reflexive "this way" delayed-trigger building block — RUNTIME
//! coverage for the two S25 cards that motivated it, driven through the real
//! cast pipeline (`GameScenario` / `SpellCast::resolve`).
//!
//!   * **Prishe's Wanderings** {2}{G} — "Search your library for a basic land
//!     card or Town card, put it onto the battlefield tapped, then shuffle. When
//!     you search your library this way, put a +1/+1 counter on target creature
//!     you control."
//!   * **Rhino's Rampage** {R/G} — "Target creature you control gets +1/+0 until
//!     end of turn. It fights target creature an opponent controls. When excess
//!     damage is dealt to the creature an opponent controls this way, destroy up
//!     to one target noncreature artifact with mana value 3 or less."
//!
//! Both reflexive clauses lower to `Effect::CreateDelayedTrigger` with the
//! `DelayedTriggerLifetime::Reflexive` lifetime (parser round-trip proven in
//! `oracle_effect::tests`). The distinguishing runtime behavior of `Reflexive`
//! (vs. the lingering CR 603.7b `ThisTurn` one-shot) is that an unmatched
//! reflexive is DISCARDED on its creation batch rather than left pending for a
//! later same-turn matching event — proven by the discard tests below, each of
//! which flips if the lifetime is reverted to `ThisTurn`.
//!
//! CR ANCHORS:
//!   * CR 603.12 — reflexive triggered abilities are checked immediately after
//!     creation against earlier-in-resolution events; one shot on that batch.
//!   * CR 701.23 — Search. CR 120.10 — excess damage. CR 611.2c — the +1/+0 EOT
//!     set is fixed at resolution.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::DelayedTriggerCondition;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::statics::{ProhibitionScope, StaticMode};
use engine::types::zones::Zone;

const PRISHE_ORACLE: &str = "Search your library for a basic land card or Town card, \
put it onto the battlefield tapped, then shuffle. When you search your library this way, \
put a +1/+1 counter on target creature you control.";

const RHINO_ORACLE: &str = "Target creature you control gets +1/+0 until end of turn. \
It fights target creature an opponent controls. When excess damage is dealt to the \
creature an opponent controls this way, destroy up to one target noncreature artifact \
with mana value 3 or less.";

fn red_units(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect()
}

fn green_units(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
        .collect()
}

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn lingering_reflexive_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .delayed_triggers
        .iter()
        .filter(|dt| matches!(dt.condition, DelayedTriggerCondition::WhenNextEvent { .. }))
        .count()
}

// ===========================================================================
// Prishe's Wanderings
// ===========================================================================

/// CR 603.12 + CR 701.23: casting Prishe searches the library (emitting
/// SearchedLibrary in the SAME resolution that creates the reflexive), so the
/// reflexive fires immediately and puts a +1/+1 counter on the target creature.
///
/// REVERT-PROBE: on revert of the detector the reflexive clause parses to
/// `Effect::unimplemented`, no delayed trigger is created, and the creature keeps
/// 0 counters — this `assert_eq!(.., 1)` flips.
#[test]
fn prishe_wanderings_search_this_way_places_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ally = scenario.add_creature(P0, "Counter Target", 2, 2).id();
    // Any library card so a search occurs; SearchedLibrary fires regardless of
    // whether a legal basic is found (CR 701.23a).
    scenario.add_card_to_library_top(P0, "Some Card");

    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Prishe's Wanderings", false, PRISHE_ORACLE);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 2,
    });
    let prishe = builder.id();
    scenario.with_mana_pool(P0, green_units(6));

    let mut runner = scenario.build();
    assert_eq!(p1p1(&runner, ally), 0, "creature starts with no counters");

    runner
        .cast(prishe)
        .search_first_legal()
        .target_object(ally)
        .resolve();

    assert_eq!(
        p1p1(&runner, ally),
        1,
        "the search-this-way reflexive must put a +1/+1 counter on the target creature"
    );
}

/// CR 603.12 + CR 701.23 + CR 609.3: with a `CantSearchLibrary` static muzzling
/// the search, Prishe's library search is a no-op that emits NO SearchedLibrary
/// event, so the reflexive is UNMATCHED on its creation batch. A `Reflexive`
/// lifetime discards it there: no counter lands and no delayed trigger lingers.
///
/// REVERT-PROBE (discriminating): reverting the lifetime to `ThisTurn` leaves the
/// unmatched reflexive LINGERING in `delayed_triggers` (CR 603.7b "next time"),
/// so `lingering_reflexive_count == 1` and the `assert_eq!(.., 0)` below flips.
/// This is a post-resolution game-state assertion driven through the real cast
/// pipeline, not an AST-shape check.
#[test]
fn prishe_wanderings_muzzled_search_discards_unmatched_reflexive() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ally = scenario.add_creature(P0, "Counter Target", 2, 2).id();
    // A permanent whose static muzzles every library search (CR 701.23 / 609.3).
    scenario
        .add_creature(P0, "Muzzle", 1, 1)
        .with_static(StaticMode::CantSearchLibrary {
            cause: ProhibitionScope::AllPlayers,
        });
    scenario.add_card_to_library_top(P0, "Some Card");

    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Prishe's Wanderings", false, PRISHE_ORACLE);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 2,
    });
    let prishe = builder.id();
    scenario.with_mana_pool(P0, green_units(6));

    let mut runner = scenario.build();

    runner
        .cast(prishe)
        .search_first_legal()
        .target_object(ally)
        .resolve();

    assert_eq!(
        p1p1(&runner, ally),
        0,
        "the muzzled search emits no SearchedLibrary, so the reflexive cannot fire"
    );
    assert_eq!(
        lingering_reflexive_count(&runner),
        0,
        "an unmatched Reflexive must be DISCARDED on its creation batch — reverting to \
         ThisTurn would leave it lingering (count 1)"
    );
}

// ===========================================================================
// Rhino's Rampage
// ===========================================================================

/// CR 603.12 + CR 120.10: a 3/1 (after +1/+0) fighting a 1/1 deals 2 excess to
/// the fought creature this way, so the reflexive fires and destroys the target
/// MV-0 noncreature artifact.
///
/// REVERT-PROBE: on revert the reflexive clause is `Effect::unimplemented`, no
/// delayed trigger is created, and the artifact stays on the battlefield — this
/// `Zone::Graveyard` assertion flips.
#[test]
fn rhinos_rampage_excess_this_way_destroys_artifact() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mine = scenario.add_creature(P0, "My Fighter", 2, 1).id();
    let opp = scenario.add_creature(P1, "Opp Fighter", 1, 1).id();
    let artifact = scenario
        .add_creature(P1, "Trinket", 0, 1)
        .as_artifact()
        .id();

    let mut builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Rhino's Rampage", false, RHINO_ORACLE);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    });
    let rhino = builder.id();
    scenario.with_mana_pool(P0, red_units(2));

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&artifact)
        .unwrap()
        .mana_cost = ManaCost::Cost {
        shards: vec![],
        generic: 0,
    };

    runner
        .cast(rhino)
        .target_object(mine)
        .target_object(opp)
        .target_object(artifact)
        .resolve();

    assert_eq!(
        runner.state().objects[&artifact].zone,
        Zone::Graveyard,
        "excess damage to the fought creature this way must fire the reflexive and \
         destroy the target noncreature artifact"
    );
}

/// CR 603.12 + CR 120.10 (DISCRIMINATING discard): a 2/1 (after +1/+0) fighting a
/// 3/3 deals NO excess to the fought creature, so Rhino's reflexive is unmatched
/// on its creation batch and — being `Reflexive` — is DISCARDED. A LATER
/// same-turn excess-damage event to that 3/3 (a 5-damage burn) must therefore NOT
/// destroy the artifact.
///
/// REVERT-PROBE (the discriminating assertion): reverting the lifetime to
/// `ThisTurn` leaves the unmatched reflexive lingering; the later burn's excess
/// damage to the same opponent creature would then fire it and destroy the
/// artifact. `Zone::Battlefield` here flips to `Zone::Graveyard` on revert.
#[test]
fn rhinos_rampage_no_fight_excess_then_later_excess_does_not_destroy() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mine = scenario.add_creature(P0, "My Fighter", 1, 1).id();
    let opp = scenario.add_creature(P1, "Opp Fighter", 3, 3).id();
    let artifact = scenario
        .add_creature(P1, "Trinket", 0, 1)
        .as_artifact()
        .id();

    let mut rhino_builder =
        scenario.add_spell_to_hand_from_oracle(P0, "Rhino's Rampage", false, RHINO_ORACLE);
    rhino_builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    });
    let rhino = rhino_builder.id();

    // A second same-turn source of excess damage to the fought creature.
    let mut burn_builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Boulder Burn",
        true,
        "Boulder Burn deals 5 damage to target creature.",
    );
    burn_builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 0,
    });
    let burn = burn_builder.id();

    scenario.with_mana_pool(P0, red_units(4));

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&artifact)
        .unwrap()
        .mana_cost = ManaCost::Cost {
        shards: vec![],
        generic: 0,
    };

    runner
        .cast(rhino)
        .target_object(mine)
        .target_object(opp)
        .target_object(artifact)
        .resolve();

    // Rhino dealt only 2 (non-lethal) to the 3/3 — zero excess — so nothing was
    // destroyed and, with the Reflexive discard, nothing lingers.
    assert_eq!(
        runner.state().objects[&artifact].zone,
        Zone::Battlefield,
        "no excess on the fight → artifact must survive Rhino's resolution"
    );
    assert_eq!(
        lingering_reflexive_count(&runner),
        0,
        "an unmatched Reflexive is discarded on its creation batch"
    );

    // Later same-turn excess damage to the SAME opponent creature.
    runner.cast(burn).target_object(opp).resolve();

    assert_eq!(
        runner.state().objects[&artifact].zone,
        Zone::Battlefield,
        "the discarded reflexive must NOT fire on a later same-turn excess-damage event; \
         reverting to ThisTurn would let it linger and destroy the artifact here"
    );
}

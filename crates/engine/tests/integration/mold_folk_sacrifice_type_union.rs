//! Mold Folk — "Mold Harvest — {1}, Sacrifice another creature or an artifact:
//! Put a +1/+1 counter on this creature."
//!
//! CR 205.2a (a cost filter may be a two-leg card-TYPE union) + CR 601.2h (the
//! activation cost is paid with permanents matching that filter) + CR 701.16a
//! (sacrifice).
//!
//! The right conjunct of the union leads with an indefinite article, and the
//! shared type-phrase grammar deliberately leaves that tail as remainder — the
//! same surface is an elided-verb clause elsewhere ("you control a land creature
//! or a land entered the battlefield this turn", Earth Rumble Wrestlers). The
//! sacrifice-cost parser now opts into the union reading via
//! `parse_target_with_article_led_type_union`, so the cost's filter is
//! `Or[Creature+Another, Artifact+Another]` rather than `Creature+Another` alone.
//!
//! Before the fix the artifact leg was dropped, so a controller holding only an
//! artifact could not pay a cost the card plainly allows. This drives the real
//! activation pipeline (`GameAction::ActivateAbility` → cost payment → stack →
//! resolution) rather than asserting a parsed filter shape.

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MOLD_FOLK: &str = "Lifelink\n\
     Mold Harvest — {1}, Sacrifice another creature or an artifact: Put a +1/+1 counter on this creature.";

/// The artifact leg is REACHABLE: with no other creature on the battlefield, the
/// only legal sacrifice is the artifact, so the ability can be activated only if
/// the union kept its second leg.
///
/// Revert-failing: with the cost filter collapsed to `Creature+Another` the cost
/// is unpayable here, the ability never resolves, and the source gains no
/// counter while the artifact stays on the battlefield.
#[test]
fn mold_folk_pays_its_cost_by_sacrificing_an_artifact() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Mold Folk", 2, 2, MOLD_FOLK)
        .id();
    let artifact = scenario.add_artifact_from_oracle(P0, "Bone Shard", "").id();
    // CR 602.1a: {1} for the activation cost (everything before the colon).
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();

    // CR 601.2h: the artifact is submitted as the cost payment. The engine
    // accepts it only if the cost's filter actually admits an artifact.
    let outcome = runner.activate(source, 0).pay_with(&[artifact]).resolve();
    let state = outcome.state();

    assert_eq!(
        state.objects[&source]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the ability must resolve, which it can only do if the artifact was a legal sacrifice"
    );
    assert_eq!(
        state.objects[&artifact].zone,
        Zone::Graveyard,
        "the artifact leg of the cost's type union must be sacrificeable"
    );
}

/// PAIRED CONTROL, the leg that already worked: with a creature available and no
/// artifact, the same ability still pays by sacrificing the creature. Proves the
/// union widened the filter rather than replacing one leg with the other.
#[test]
fn mold_folk_still_pays_its_cost_by_sacrificing_a_creature() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(P0, "Mold Folk", 2, 2, MOLD_FOLK)
        .id();
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();

    let outcome = runner.activate(source, 0).pay_with(&[fodder]).resolve();
    let state = outcome.state();

    assert_eq!(
        state.objects[&source]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the creature leg must keep working"
    );
    assert_eq!(
        state.objects[&fodder].zone,
        Zone::Graveyard,
        "the creature was the sacrifice"
    );
    assert_eq!(
        state.objects[&source].zone,
        Zone::Battlefield,
        "CR 109.4: \"another\" excludes the source itself"
    );
}

//! Mold Folk — "Mold Harvest — {1}, Sacrifice another creature or an artifact:
//! Put a +1/+1 counter on this creature."
//!
//! CR 205.2a (a cost filter may be a two-leg card-TYPE union) + CR 601.2h (the
//! activation cost is paid with permanents matching that filter) + CR 701.21a
//! (sacrifice).
//!
//! The right conjunct of the union leads with an indefinite article, and the
//! shared type-phrase grammar deliberately leaves that tail as remainder — the
//! same surface is an elided-verb clause elsewhere ("you control a land creature
//! or a land entered the battlefield under your control this turn", Earth Rumble Wrestlers). The
//! sacrifice-cost parser now opts into the union reading via
//! `fold_article_led_type_union`, so the cost's filter is
//! `Or[Creature+Another, Artifact]` rather than `Creature+Another` alone.
//!
//! Note the ASYMMETRY, which is the rules-load-bearing part: "another" scopes only
//! the LEFT conjunct. The right one carries its own determiner ("an artifact"), so
//! it is not "another artifact" and the source may pay with ITSELF once it is an
//! artifact. Official rulings — Elite Headhunter (2019-10-04): "If Elite Headhunter
//! somehow becomes an artifact, you can sacrifice it to pay the cost of its
//! activated ability"; Gut, True Soul Zealot (2022-06-10): "If Gut somehow becomes
//! an artifact, you may sacrifice it to its own ability." The third test below
//! drives exactly that.
//!
//! Before the fix the artifact leg was dropped, so a controller holding only an
//! artifact could not pay a cost the card plainly allows. This drives the real
//! activation pipeline (`GameAction::ActivateAbility` → cost payment → stack →
//! resolution) rather than asserting a parsed filter shape.

use engine::game::scenario::{GameScenario, P0};
use engine::types::card_type::CoreType;
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
        .add_creature_from_oracle(P0, "Mold Folk", 1, 1, MOLD_FOLK)
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
        .add_creature_from_oracle(P0, "Mold Folk", 1, 1, MOLD_FOLK)
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
        "\"another\" excludes the source itself"
    );
}

/// CR 205.2a + the Elite Headhunter / Gut rulings: "another" scopes only the LEFT
/// conjunct, so once the source itself is an artifact it satisfies the RIGHT leg
/// and may be sacrificed to its own ability.
///
/// This is the discriminating case for the `Another` scoping, and it is reachable
/// in real games — Liquimetal Coating and Mycosynth Lattice both make a creature
/// an artifact "in addition to its other types". (Not Karn, Silver Golem: that
/// one targets a NONcreature artifact and makes it a creature, the opposite
/// direction.)
///
/// GUARD-FAILING: distribute `FilterProp::Another` onto the article-led right leg
/// (i.e. fold the union *before* applying `another`, as an earlier revision did)
/// and the source matches neither leg — `matches_filter_prop`'s `Another` arm
/// resolves to `!source_is_current_object(..)` on this path — so
/// `find_eligible_sacrifice_targets` returns nothing, the cost is unpayable with
/// nothing else on the battlefield, and the activation is refused.
#[test]
fn mold_folk_may_sacrifice_itself_once_it_is_an_artifact() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Set as a fixture rather than parsed from a type-granting card: the subject
    // here is the COST FILTER, and routing through a continuous effect would put
    // the layer system in the path of a test that is not about it.
    // `as_artifact` REPLACES the creature type; `as_creature` puts it back
    // idempotently and re-syncs the base types, so the fixture is an artifact
    // CREATURE — the "in addition to its other types" state Liquimetal Coating and
    // Mycosynth Lattice produce, and the shape Street Urchin's own 2022-06-10
    // ruling describes ("If your commander is an artifact creature, you may
    // sacrifice it to pay the cost of this ability"). A pure artifact would
    // discriminate the same way but leave the both-types arm unexercised.
    let source = {
        let mut b = scenario.add_creature_from_oracle(P0, "Mold Folk", 1, 1, MOLD_FOLK);
        b.as_artifact();
        b.as_creature();
        b.id()
    };
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

    let core = &runner.state().objects[&source].card_types.core_types;
    assert!(
        core.contains(&CoreType::Artifact) && core.contains(&CoreType::Creature),
        "reach-guard: the source must be an artifact CREATURE, or this proves \
         something narrower than the rulings describe: {core:?}"
    );

    // Nothing else is on the battlefield, so the ONLY legal payment is the source
    // itself via the artifact leg.
    let outcome = runner.activate(source, 0).pay_with(&[source]).resolve();
    let state = outcome.state();
    assert_eq!(
        state.objects[&source].zone,
        Zone::Graveyard,
        "the source satisfies the article-led artifact leg and may pay with itself"
    );
}

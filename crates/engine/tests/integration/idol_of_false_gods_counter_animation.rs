//! Issue #8395 — Idol of False Gods: the animation clause was silently dropped
//! from a compound continuous static, so the artifact never became a creature.
//!
//! Oracle text (verbatim, Scryfall `cards/named?exact=`):
//!   "{1}{C}, {T}: Create a 0/1 colorless Eldrazi Spawn creature token with
//!    \"Sacrifice this token: Add {C}.\"
//!    Whenever another Eldrazi you control dies, put a +1/+1 counter on this
//!    artifact.
//!    As long as this artifact has eight or more +1/+1 counters on it, it's a
//!    0/0 creature in addition to its other types and it has annihilator 2."
//!
//! # The defect
//!
//! The third line reaches the static dispatcher only AFTER
//! `try_split_inverted_as_long_as` rewrites the inverted "As long as <cond>,
//! <effect>" form into canonical "<effect> as long as <cond>" order. In that
//! order the conjunct's `" has "` (from "and it **has** annihilator 2") precedes
//! the gate's `" as long as "`, so a legacy arm keyed on the FIRST `" has "`
//! claimed the line, sliced the keyword out of the middle, discarded everything
//! before it — the entire animation clause — and hardcoded `SelfRef`. The card
//! exported `[AddKeyword{Annihilator(2)}]` and nothing else, so Idol stayed a
//! non-creature artifact forever.
//!
//! # How this test discriminates
//!
//! With the fix reverted the parse emits only `AddKeyword{Annihilator(2)}`;
//! `AddType{Creature}` is absent and the "is a Creature" assertion in the ON
//! state fails immediately.
//!
//! The Annihilator assertion passes BOTH ways by design — that is exactly what
//! makes it the reach-guard rather than independent coverage: its presence in
//! the same run proves the static was claimed and applied at all, so the missing
//! Creature type is a real drop and not a line that simply vanished.
//!
//! The OFF state at seven counters is the second guard: the same object is
//! observed not-a-creature and then a creature, so the ON assertions cannot pass
//! vacuously. The scenario contains no other type-changing effect, so nothing
//! else could make Idol a creature.
//!
//! Wizards' own shipped ruling on this card confirms the intended end state:
//! "…it will be an Eldrazi kindred artifact creature. This is because effects
//! that change an object's types are always applied before effects that remove
//! abilities."

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const IDOL_ORACLE: &str = "{1}{C}, {T}: Create a 0/1 colorless Eldrazi Spawn creature token with \"Sacrifice this token: Add {C}.\"\n\
Whenever another Eldrazi you control dies, put a +1/+1 counter on this artifact.\n\
As long as this artifact has eight or more +1/+1 counters on it, it's a 0/0 creature in addition to its other types and it has annihilator 2.";

/// Force a full layer recomputation so every read below observes the CR 613
/// pipeline's current output rather than a cached value.
fn refresh(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Mark lethal damage on `victim` so the next state-based-action check kills it.
///
/// CR 704.5g: a creature with damage marked on it greater than or equal to its
/// toughness is destroyed. Marking the damage is a BOARD PREMISE; the death
/// itself, the resulting dies event, Idol's trigger, and the counter placement
/// all run through the production SBA + priority machinery, so the engine — not
/// this test — puts the eighth counter on Idol.
fn mark_lethal(runner: &mut GameRunner, victim: ObjectId) {
    let obj = runner
        .state_mut()
        .objects
        .get_mut(&victim)
        .expect("victim present");
    obj.damage_marked = obj.toughness.unwrap_or(1).max(1) as u32;
}

#[test]
fn idol_of_false_gods_animates_at_eight_counters_and_keeps_annihilator() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Idol of False Gods, built from verbatim Oracle text through the real parse
    // + synthesis pipeline. Seven +1/+1 counters are the BOARD PREMISE, not the
    // behavior under test — the eighth is placed by the engine below.
    let idol = scenario
        .add_artifact_from_oracle(P0, "Idol of False Gods", IDOL_ORACLE)
        .with_subtypes(vec!["Eldrazi"])
        .with_plus_counters(7)
        .id();

    // Another Eldrazi P0 controls. Killing it is what drives Idol's own printed
    // dies-trigger, so the eighth counter arrives through the engine's path.
    let spawn = scenario
        .add_creature(P0, "Eldrazi Spawn", 0, 1)
        .with_subtypes(vec!["Eldrazi", "Spawn"])
        .id();

    let mut runner = scenario.build();

    // ---------------- OFF state at seven counters ----------------
    // CR 611.3a: a static ability's continuous effect is never "locked in" — it
    // applies exactly while its condition holds.
    refresh(&mut runner);
    assert_eq!(
        plus_counters(&runner, idol),
        7,
        "board premise: the fixture starts one counter short of the gate"
    );
    {
        let obj = &runner.state().objects[&idol];
        assert!(
            !obj.card_types.core_types.contains(&CoreType::Creature),
            "at seven counters the gate is false, so Idol is not a creature; types = {:?}",
            obj.card_types
        );
        assert!(
            obj.card_types.core_types.contains(&CoreType::Artifact),
            "reach-guard: this is the Idol object and it is an artifact; types = {:?}",
            obj.card_types
        );
        assert!(
            !obj.has_keyword(&Keyword::Annihilator(2)),
            "at seven counters the gate is false, so annihilator is not granted"
        );
        // CR 208.3: a noncreature permanent has no power or toughness at all.
        assert_eq!(
            obj.power, None,
            "CR 208.3: a noncreature permanent has no power"
        );
        assert_eq!(
            obj.toughness, None,
            "CR 208.3: a noncreature permanent has no toughness"
        );
    }

    // ---------------- Drive the eighth counter through the engine ----------------
    // Mark lethal damage (board premise), then hand priority to the production
    // machinery: SBA (CR 704.5g) destroys the Spawn, the dies event fires Idol's
    // printed trigger, and resolving it places the counter.
    mark_lethal(&mut runner, spawn);
    runner.pass_both_players();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&spawn].zone,
        Zone::Graveyard,
        "reach-guard: the other Eldrazi actually died, so the dies-trigger had an event to see"
    );
    assert_eq!(
        plus_counters(&runner, idol),
        8,
        "CR 122.1: Idol's own printed trigger placed the eighth counter — the ENGINE put it \
         there, not the test fixture"
    );

    // ---------------- ON state after layers ----------------
    refresh(&mut runner);
    let obj = &runner.state().objects[&idol];

    // Reach-guard, and V2 of the verification matrix: this assertion passes both
    // before and after the fix. Its role is to prove the static was CLAIMED and
    // APPLIED, so the type assertion below is measuring a dropped clause rather
    // than a line that never parsed.
    assert!(
        obj.has_keyword(&Keyword::Annihilator(2)),
        "CR 613.1f (Layer 6) + CR 702.86: annihilator 2 is granted at eight counters; \
         keywords = {:?}",
        obj.keywords
    );

    // THE DISCRIMINATING ASSERTION. Absent before the fix.
    assert!(
        obj.card_types.core_types.contains(&CoreType::Creature),
        "CR 613.1d (Layer 4): at eight counters Idol becomes a creature; types = {:?}",
        obj.card_types
    );

    // CR 205.1b: "in addition to its other types" RETAINS the prior types. The
    // conjunction is the evidence — a `SetCardTypes` regression would drop these
    // while the Creature assertion above still passed.
    assert!(
        obj.card_types.core_types.contains(&CoreType::Artifact),
        "CR 205.1b: Idol remains an artifact while it is also a creature; types = {:?}",
        obj.card_types
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Eldrazi"),
        "CR 205.1b: Idol remains an Eldrazi while it is also a creature; types = {:?}",
        obj.card_types
    );

    // CR 613.4b (Layer 7b) sets base 0/0; CR 613.4c (Layer 7c) then applies the
    // eight +1/+1 counters — 8/8, in that order.
    //
    // NOTE: this pair is deliberately NOT counted as discriminating on its own.
    // Idol's printed P/T is null, so a reading of 8/8 can arise from the counters
    // alone; the layer-order claim it documents is carried by the leaf-level
    // parser tests. It is asserted here because the ON state should be stated in
    // full, not because it discriminates.
    assert_eq!(
        obj.power,
        Some(8),
        "CR 613.4b then CR 613.4c: base 0 set at 7b, +8 from counters at 7c"
    );
    assert_eq!(
        obj.toughness,
        Some(8),
        "CR 613.4b then CR 613.4c: base 0 set at 7b, +8 from counters at 7c"
    );
}

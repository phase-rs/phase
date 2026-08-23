//! Make Your Move ({2}{W} Instant): "Destroy target artifact, enchantment, or
//! creature with power 4 or greater." Exorcise is the same shape with Exile.
//!
//! CR 208.1: power and toughness are the two numbers printed on a CREATURE
//! card. CR 208.3: a noncreature permanent has NO power or toughness, even if
//! it's a card with a power and toughness printed on it (such as a Vehicle).
//! The postnominal "with power 4 or greater" therefore restricts only the
//! creature disjunct — an artifact or enchantment is a legal target regardless
//! of power. CR 115.1a: "target [something]" identifies the objects the spell
//! may affect; CR 601.2c: those targets are chosen as the spell is cast.
//!
//! Buggy parse (before this fix):
//!   Or[ Typed{[Artifact],    [PtComparison{Power,GE,4}]},
//!       Typed{[Enchantment], [PtComparison{Power,GE,4}]},
//!       Typed{[Creature],    [PtComparison{Power,GE,4}]} ]
//! Because `game::filter::pt_value_from_pair` reads `power.unwrap_or(0)` for a
//! noncreature, the artifact and enchantment legs matched nothing at all — the
//! Disenchant half of the card was dead.
//!
//! Fixed parse (matches the already-correct Broken Wings / Vivien Reid shape):
//!   Or[ Typed{[Artifact]}, Typed{[Enchantment]},
//!       Typed{[Creature], [PtComparison{Power,GE,4}]} ]
//!
//! The spell is built from verbatim Oracle text rather than the card database:
//! `data/card-data.json` is gitignored and the integration fixture is a
//! committed snapshot, so a DB-backed test would read a stale pre-fix parse and
//! go green while the card stayed broken.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::targeting::find_legal_targets;
use engine::game::zones::create_object;
use engine::types::ability::{Effect, TargetFilter, TargetRef};
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Scryfall Oracle text (oracle_id 8226f31d-6f51-49c3-87f7-0c68f7f4f9ce).
const MAKE_YOUR_MOVE: &str =
    "Destroy target artifact, enchantment, or creature with power 4 or greater.";

/// `{2}{W}` floating so no `ManaPayment` window surfaces during the cast.
fn make_your_move_mana() -> Vec<ManaUnit> {
    let mut pool = vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])];
    for _ in 0..2 {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    pool
}

/// `GameScenario` has no artifact builder, so mirror the
/// `issue_2941_vivien_reid.rs` idiom: create the object, then push the core
/// type. Power/toughness stay `None` — CR 208.3, a noncreature has none.
fn add_noncreature_permanent(
    state: &mut GameState,
    card_id: u64,
    player: PlayerId,
    name: &str,
    core_type: CoreType,
) -> ObjectId {
    let oid = create_object(
        state,
        CardId(card_id),
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&oid).expect("just created");
    obj.card_types.core_types.push(core_type);
    obj.base_card_types = obj.card_types.clone();
    oid
}

/// Build a runner with Make Your Move in P0's hand and enough floating mana.
fn setup() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Make Your Move", true, MAKE_YOUR_MOVE)
        .id();
    scenario.with_mana_pool(P0, make_your_move_mana());
    (scenario, spell)
}

/// Pull the parsed `Destroy` target filter straight off the spell object, so the
/// runtime rows below and the AST row assert against the same value.
fn destroy_target_filter(state: &GameState, spell: ObjectId) -> TargetFilter {
    state
        .objects
        .get(&spell)
        .expect("spell object")
        .abilities
        .iter()
        .find_map(|ability| match &*ability.effect {
            Effect::Destroy { target, .. } => Some(target.clone()),
            _ => None,
        })
        .expect("Make Your Move should parse to a targeted Destroy")
}

/// Row 11: a powerless noncreature artifact is a legal target and is destroyed.
/// This is the primary claim; on revert the artifact is not a legal target
/// (CR 208.3 gives it no power, so `PtComparison{Power,GE,4}` reads 0) and the
/// cast cannot complete.
#[test]
fn powerless_artifact_is_a_legal_target_and_is_destroyed() {
    let (scenario, spell) = setup();
    let mut runner: GameRunner = scenario.build();
    let artifact = add_noncreature_permanent(
        runner.state_mut(),
        9001,
        P1,
        "Opp Artifact",
        CoreType::Artifact,
    );

    let outcome = runner.cast(spell).target_objects(&[artifact]).resolve();
    outcome.assert_zone(&[artifact], Zone::Graveyard);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "spell must resolve fully, not halt: {:?}",
        outcome.final_waiting_for()
    );
}

/// Row 15: the enchantment leg is genuinely reachable at runtime, not just
/// clean in the AST. A real 0-power enchantment (no Aura attachment).
#[test]
fn powerless_enchantment_is_a_legal_target_and_is_destroyed() {
    let (scenario, spell) = setup();
    let mut runner = scenario.build();
    let enchantment = add_noncreature_permanent(
        runner.state_mut(),
        9002,
        P1,
        "Opp Enchantment",
        CoreType::Enchantment,
    );

    let outcome = runner.cast(spell).target_objects(&[enchantment]).resolve();
    outcome.assert_zone(&[enchantment], Zone::Graveyard);
}

/// Row 13: the creature leg's restriction still works in the positive
/// direction — a 4-power creature is destroyed. Paired in-file with row 12 so
/// that negative cannot pass because the spell is simply broken.
#[test]
fn four_power_creature_is_a_legal_target_and_is_destroyed() {
    let (mut scenario, spell) = setup();
    let creature = scenario.add_creature(P1, "Big Creature", 4, 4).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_objects(&[creature]).resolve();
    outcome.assert_zone(&[creature], Zone::Graveyard);
}

/// Row 12: the creature leg keeps its restriction — the fix did not simply drop
/// it. A 2-power creature is NOT among the legal targets. Asserted through
/// `find_legal_targets` rather than a driver panic so the failure mode is an
/// assertion, and paired with a positive reach-guard in the same test proving
/// the filter is live (the 4-power creature IS legal).
#[test]
fn two_power_creature_is_not_a_legal_target_but_four_power_is() {
    let (mut scenario, spell) = setup();
    let small = scenario.add_creature(P1, "Small Creature", 2, 2).id();
    let big = scenario.add_creature(P1, "Big Creature", 4, 4).id();
    let runner = scenario.build();

    let filter = destroy_target_filter(runner.state(), spell);
    let legal = find_legal_targets(runner.state(), &filter, P0, spell);

    assert!(
        !legal.contains(&TargetRef::Object(small)),
        "2-power creature must not satisfy 'power 4 or greater': {legal:?}"
    );
    // Positive reach-guard: the creature leg is live, so the negative above is
    // not vacuous.
    assert!(
        legal.contains(&TargetRef::Object(big)),
        "4-power creature must satisfy 'power 4 or greater': {legal:?}"
    );
}

/// Row 14: the single case that separates the two readings. A 2/2 ARTIFACT
/// creature has power 2, so it fails the creature disjunct — but CR 205.2b (an
/// object with more than one card type satisfies any effect applying to any of
/// them) plus CR 115.1a mean it satisfies the bare `artifact` disjunct. Illegal
/// under the buggy distributed parse, legal under the leg-local parse.
#[test]
fn two_power_artifact_creature_is_legal_via_the_bare_artifact_leg() {
    let (mut scenario, spell) = setup();
    let artifact_creature = scenario.add_creature(P1, "Small Servo", 2, 2).id();
    let mut runner = scenario.build();
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&artifact_creature)
            .expect("artifact creature");
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.base_card_types = obj.card_types.clone();
    }

    let outcome = runner
        .cast(spell)
        .target_objects(&[artifact_creature])
        .resolve();
    outcome.assert_zone(&[artifact_creature], Zone::Graveyard);
}

/// Row 16: the same binding for a leg named ONLY by a noncreature SUBTYPE.
/// CR 205.3d + CR 205.3g: Vehicle is an artifact type, so "creature or Vehicle"
/// pins the artifact card type on the second disjunct even though no card-type
/// word is printed there. CR 301.7a: a Vehicle has its printed power only while
/// it's also a creature, so an uncrewed Vehicle has no power (CR 208.3) — the
/// restriction must bind to the creature disjunct, leaving the Vehicle leg
/// targetable. Under the buggy distribution the Vehicle leg carries
/// `PtComparison{Power,GE,4}` and `pt_value_from_pair`'s `power.unwrap_or(0)`
/// makes it match nothing, so this assertion flips on revert.
///
/// No printed card prints this exact wording today, so the Oracle text here is a
/// structural fixture for the class (`Suit Up` prints the same "creature or
/// Vehicle" disjunct without the power clause), not a card reproduction.
#[test]
fn uncrewed_vehicle_leg_is_targetable_but_small_creature_is_not() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Vehicle Class Guard",
            true,
            "Destroy target creature or Vehicle with power 4 or greater.",
        )
        .id();
    scenario.with_mana_pool(P0, make_your_move_mana());
    let small = scenario.add_creature(P1, "Small Creature", 2, 2).id();
    let mut runner: GameRunner = scenario.build();

    // An uncrewed Vehicle: an artifact with the Vehicle subtype and NO live
    // power/toughness (CR 301.7a + CR 208.3).
    let vehicle = create_object(
        runner.state_mut(),
        CardId(9003),
        P1,
        "Opp Vehicle".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&vehicle)
            .expect("just created");
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Vehicle".to_string());
        obj.base_card_types = obj.card_types.clone();
    }

    // Positive reach-guard on the other half, taken before the cast: the
    // creature disjunct still enforces the restriction, so "the Vehicle is
    // legal" below is not "the filter matches everything".
    let filter = destroy_target_filter(runner.state(), spell);
    let legal = find_legal_targets(runner.state(), &filter, P0, spell);
    assert!(
        legal.contains(&TargetRef::Object(vehicle)),
        "CR 208.3: an uncrewed Vehicle has no power, so the Vehicle disjunct must \
         be unrestricted and the Vehicle legal: {legal:?}"
    );
    assert!(
        !legal.contains(&TargetRef::Object(small)),
        "the creature leg must still reject a 2-power creature: {legal:?}"
    );

    let outcome = runner.cast(spell).target_objects(&[vehicle]).resolve();
    outcome.assert_zone(&[vehicle], Zone::Graveyard);
}

/// Row 17: the EXCLUSION-ONLY leg shape. CR 205.4b: "nonartifact permanent"
/// scopes the disjunct by exclusion, producing `[Permanent, Non(Artifact)]` —
/// no creature noun anywhere in it. An enchantment satisfies that leg and CR
/// 208.3 gives it no power, so distributing "with power 4 or greater" there
/// makes `pt_value_from_pair`'s `power.unwrap_or(0)` reject every noncreature
/// nonartifact permanent — silently deleting the entire first half of the
/// disjunction, exactly the Make Your Move defect wearing a negation.
///
/// NOTE ON THE DISCRIMINATOR. The natural pairing — "a small creature must be
/// rejected" — is NOT assertable for this shape, and asserting it would be
/// wrong: a 2/2 creature IS a nonartifact permanent, so once the first leg is
/// correctly unrestricted the small creature becomes legal THROUGH THAT LEG
/// (CR 115.1a). That is the printed meaning of "target nonartifact permanent or
/// creature with power 4 or greater" — the first disjunct carries no power
/// restriction at all. The reach-guard therefore uses an ARTIFACT, which both
/// legs exclude (`distribute_neg_type_filters_to_or` shares `Non(Artifact)`
/// across the disjunction), proving the filter still discriminates rather than
/// matching everything.
///
/// No printed card prints this wording; like the Vehicle row above it is a
/// structural fixture for the class.
#[test]
fn exclusion_only_leg_admits_a_powerless_enchantment_but_still_excludes_artifacts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Negation Class Guard",
            true,
            "Destroy target nonartifact permanent or creature with power 4 or greater.",
        )
        .id();
    scenario.with_mana_pool(P0, make_your_move_mana());
    let mut runner: GameRunner = scenario.build();

    // A powerless, noncreature, nonartifact permanent: satisfies the exclusion
    // leg and nothing else.
    let enchantment = add_noncreature_permanent(
        runner.state_mut(),
        9004,
        P1,
        "Opp Enchantment",
        CoreType::Enchantment,
    );
    // An artifact: excluded by `Non(Artifact)` on BOTH legs.
    let artifact = add_noncreature_permanent(
        runner.state_mut(),
        9005,
        P1,
        "Opp Artifact",
        CoreType::Artifact,
    );

    let filter = destroy_target_filter(runner.state(), spell);
    let legal = find_legal_targets(runner.state(), &filter, P0, spell);

    // The claim: flips from illegal to legal when the exclusion leg stops
    // inheriting the creature leg's power restriction.
    assert!(
        legal.contains(&TargetRef::Object(enchantment)),
        "CR 205.4b + CR 208.3: a nonartifact permanent with no power must be \
         legal through the unrestricted exclusion leg: {legal:?}"
    );
    // Reach-guard: the filter is not simply matching everything.
    assert!(
        !legal.contains(&TargetRef::Object(artifact)),
        "an artifact is excluded by Non(Artifact) on both legs: {legal:?}"
    );

    // AST row: the restriction lives on the creature leg only.
    let TargetFilter::Or { filters } = &filter else {
        panic!("expected an Or target filter, got {filter:?}");
    };
    for leg in filters {
        let TargetFilter::Typed(typed) = leg else {
            panic!("expected every leg Typed, got {leg:?}");
        };
        let has_pt = typed
            .properties
            .iter()
            .any(|p| matches!(p, engine::types::ability::FilterProp::PtComparison { .. }));
        let anchors_creature = typed
            .type_filters
            .contains(&engine::types::ability::TypeFilter::Creature);
        assert_eq!(
            has_pt, anchors_creature,
            "CR 208.1: only the creature-anchored leg may carry the power \
             restriction, got {typed:?}"
        );
    }

    let outcome = runner.cast(spell).target_objects(&[enchantment]).resolve();
    outcome.assert_zone(&[enchantment], Zone::Graveyard);
}

/// AST-shape guard mirroring `issue_2941_vivien_reid.rs`: the parsed filter must
/// match the already-correct Broken Wings shape. Complements the runtime rows —
/// they prove the legs match objects, this pins exactly which leg carries the
/// restriction.
#[test]
fn parser_binds_power_restriction_to_creature_leg_only() {
    let (scenario, spell) = setup();
    let runner = scenario.build();
    let filter = destroy_target_filter(runner.state(), spell);

    let TargetFilter::Or { filters } = &filter else {
        panic!("expected an Or target filter, got {filter:?}");
    };
    assert_eq!(filters.len(), 3, "expected three disjuncts: {filters:?}");

    for (idx, expected) in [
        (0usize, engine::types::ability::TypeFilter::Artifact),
        (1, engine::types::ability::TypeFilter::Enchantment),
    ] {
        let TargetFilter::Typed(typed) = &filters[idx] else {
            panic!("leg {idx} should be Typed: {:?}", filters[idx]);
        };
        assert!(typed.type_filters.contains(&expected));
        assert!(
            !typed
                .properties
                .iter()
                .any(|p| matches!(p, engine::types::ability::FilterProp::PtComparison { .. })),
            "CR 208.3: leg {idx} must carry no power restriction: {typed:?}"
        );
    }

    let TargetFilter::Typed(creature) = &filters[2] else {
        panic!("creature leg should be Typed: {:?}", filters[2]);
    };
    assert!(creature
        .type_filters
        .contains(&engine::types::ability::TypeFilter::Creature));
    assert!(
        creature
            .properties
            .iter()
            .any(|p| matches!(p, engine::types::ability::FilterProp::PtComparison { .. })),
        "creature leg must retain the power restriction: {creature:?}"
    );
}

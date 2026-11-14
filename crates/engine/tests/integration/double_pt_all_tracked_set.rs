//! DoublePTAll → "They" tracked-set publication (Matt R7-F4).
//!
//! Synthetic composition (no printed-card parse claim): "Double the power and
//! toughness of each creature you control. They gain haste until end of
//! turn." The parser admits the doubling head as a mass population and binds
//! "They" to `TrackedSet(0)`; the runtime must publish the resolver-selected
//! set through the tracked-set authority so the grant reaches exactly the
//! doubled members — multiple members plus an adjacent nonmember. The frozen
//! pin guards the fix SHAPE: the grant must read the frozen resolution-time
//! population, never a live filter (a late arrival matches the filter but
//! was not doubled).

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ContinuousModification, Effect, TargetFilter};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const DOUBLING_CRY: &str = "Double the power and toughness of each creature you control. They gain haste until end of turn.";

fn free_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: 0,
    }
}

fn power(runner: &GameRunner, id: ObjectId) -> Option<i32> {
    runner.state().objects[&id].power
}

fn has_haste(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].has_keyword(&Keyword::Haste)
}

/// Reach guard: the synthetic text must lower to exactly the claimed
/// composition — a `DoublePTAll` head plus a haste grant whose affected is
/// the `TrackedSet` sentinel (mirrors the Call for Aid parse test). Without
/// this, the runtime asserts below could pass against a different lowering.
fn assert_doubling_they_shape() {
    let parsed = parse_oracle_text(
        DOUBLING_CRY,
        "Doubling Cry",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let head = parsed
        .abilities
        .first()
        .expect("the synthetic text must parse to an ability");
    assert!(
        matches!(head.effect.as_ref(), Effect::DoublePTAll { .. }),
        "the head clause must lower to DoublePTAll, got {:?}",
        head.effect
    );
    let mut current = head.sub_ability.as_deref();
    let mut haste_grants = 0;
    while let Some(ability) = current {
        if let Effect::GenericEffect {
            static_abilities, ..
        } = ability.effect.as_ref()
        {
            let grants_haste = static_abilities.iter().any(|def| {
                def.modifications.iter().any(|m| {
                    matches!(
                        m,
                        ContinuousModification::AddKeyword {
                            keyword: Keyword::Haste,
                        }
                    )
                })
            });
            if grants_haste {
                haste_grants += 1;
                let affected: Vec<_> = static_abilities
                    .iter()
                    .filter_map(|def| def.affected.clone())
                    .collect();
                assert!(
                    affected
                        .iter()
                        .any(|filter| matches!(filter, TargetFilter::TrackedSet { .. })),
                    "the haste grant must bind the frozen population sentinel, got {affected:#?}"
                );
            }
        }
        current = ability.sub_ability.as_deref();
    }
    assert_eq!(haste_grants, 1, "exactly one haste grant: {parsed:#?}");
}

/// CR 701.10a + CR 611.2c + CR 608.2c (Matt R7-F4): the doubling head
/// publishes its selected population, so "They" reaches exactly the doubled
/// members — two 2/2s become 4/4s with haste, while P1's adjacent 2/2 (not
/// "you control") is untouched. Pre-fix the head doubles but publishes
/// nothing (the fallback harvests only zone changes), so nobody gains haste.
#[test]
fn doubling_they_grant_reaches_exactly_the_doubled_members() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let member_a = scenario.add_creature(P0, "Member A", 2, 2).id();
    let member_b = scenario.add_creature(P0, "Member B", 2, 2).id();
    let outsider = scenario.add_creature(P1, "Outsider", 2, 2).id();
    let cry = scenario
        .add_spell_to_hand_from_oracle(P0, "Doubling Cry", false, DOUBLING_CRY)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    assert_doubling_they_shape();

    runner.cast(cry).resolve();
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    // Reach: the head ran — both members doubled, the outsider untouched.
    assert_eq!(power(&runner, member_a), Some(4));
    assert_eq!(power(&runner, member_b), Some(4));
    assert_eq!(power(&runner, outsider), Some(2));

    // The finding: "They" names the doubled set, nothing else.
    assert!(
        has_haste(&runner, member_a),
        "member A was doubled, so They must grant it haste"
    );
    assert!(
        has_haste(&runner, member_b),
        "member B was doubled, so They must grant it haste"
    );
    assert!(
        !has_haste(&runner, outsider),
        "the outsider was not doubled, so They must not reach it"
    );
}

/// CR 611.2c (Matt R7-F4 pin): the published population is frozen at
/// resolution — a creature arriving after the cry resolved matches the head
/// filter but was never doubled, so it gains no haste either. Fails pre-fix
/// on the member leg (same root cause as the control above); post-fix it
/// pins the fix shape against a live-filter re-derivation, which would
/// wrongly hasten the latecomer.
#[test]
fn doubling_they_grant_ignores_late_arrivals() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let member = scenario.add_creature(P0, "Member", 2, 2).id();
    let latecomer = scenario
        .add_creature_to_hand(P0, "Latecomer", 2, 2)
        .with_mana_cost(free_cost())
        .id();
    let cry = scenario
        .add_spell_to_hand_from_oracle(P0, "Doubling Cry", false, DOUBLING_CRY)
        .with_mana_cost(free_cost())
        .id();
    let mut runner = scenario.build();

    assert_doubling_they_shape();

    runner.cast(cry).resolve();
    runner.advance_until_stack_empty();
    runner.cast(latecomer).resolve();
    runner.advance_until_stack_empty();
    evaluate_layers(runner.state_mut());

    assert_eq!(power(&runner, member), Some(4));
    assert_eq!(power(&runner, latecomer), Some(2));
    assert!(
        has_haste(&runner, member),
        "the doubled member must gain haste"
    );
    assert!(
        !has_haste(&runner, latecomer),
        "the late arrival was never doubled, so the frozen grant must not reach it"
    );
}

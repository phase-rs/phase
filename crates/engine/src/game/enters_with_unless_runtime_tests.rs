//! Runtime cast-pipeline coverage for the "enters with [counters] on it unless
//! [game-state condition]" replacement class (CR 614.1c). Built via the
//! `/card-test` recipe: `GameScenario` + `GameRunner::cast(..).resolve()` +
//! `CastOutcome` counter deltas. These discriminate the fix — the primary
//! assertions flip when the silently-dropped " unless " tail is reverted (the
//! counters would then always apply).

use crate::game::scenario::{GameScenario, P0, P1};
use crate::types::counter::CounterType;
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use crate::types::phase::Phase;
use crate::types::zones::Zone;

const HOTHEADED_GIANT: &str = "This creature enters with two -1/-1 counters on it \
                               unless you've cast another red spell this turn.";
const STEEL_EXEMPLAR: &str = "This creature enters with two +1/+1 counters on it \
                              unless two or more colors of mana were spent to cast it.";

fn mana(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(kind, ObjectId(0), false, vec![]); n]
}

/// CR 614.1c (the "enters with …" clause is the replacement) + CR 109.1 (the
/// "another" exclusion basis): a red spell cast earlier this turn suppresses
/// Hotheaded Giant's -1/-1 counters. REVERT DISCRIMINATOR — with the dropped-tail
/// bug the condition is null and the counters always apply (toughness 2), so this
/// `assert_counters(.., 0)` fails on revert.
#[test]
fn hotheaded_giant_suppressed_by_prior_red_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let giant = scenario
        .add_creature_to_hand_from_oracle(P0, "Hotheaded Giant", 4, 4, HOTHEADED_GIANT)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Red, 1));
    let mut runner = scenario.build();

    // A DIFFERENT red spell was cast this turn (distinct object id → "another").
    runner.state_mut().spells_cast_this_turn_by_player.insert(
        P0,
        crate::im::Vector::from(vec![crate::types::game_state::SpellCastRecord {
            colors: vec![ManaColor::Red],
            spell_object_id: Some(ObjectId(9_999)),
            ..Default::default()
        }]),
    );

    let outcome = runner.cast(giant).resolve();
    assert_eq!(
        outcome.state().objects[&giant].zone,
        Zone::Battlefield,
        "Giant must resolve onto the battlefield"
    );
    assert_eq!(
        outcome.counters(giant, CounterType::Minus1Minus1),
        0,
        "a prior red spell satisfies the unless clause → no counters"
    );
}

/// Reach-guard sibling (pairs the test above): when Hotheaded Giant is the only
/// red spell cast this turn, its OWN cast is excluded (CR 400.7 / CR 601.2i), so
/// the unless clause is false and it enters with two -1/-1 counters. Also the
/// runtime own-cast-exclusion discriminator — if the own record were counted the
/// clause would be true and this would wrongly be 0.
#[test]
fn hotheaded_giant_own_cast_not_counted_applies_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let giant = scenario
        .add_creature_to_hand_from_oracle(P0, "Hotheaded Giant", 4, 4, HOTHEADED_GIANT)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Red, 1));
    let mut runner = scenario.build();

    let outcome = runner.cast(giant).resolve();
    assert_eq!(
        outcome.state().objects[&giant].zone,
        Zone::Battlefield,
        "Giant must resolve onto the battlefield"
    );
    assert_eq!(
        outcome.counters(giant, CounterType::Minus1Minus1),
        2,
        "Giant's own cast does not count as 'another red spell' → counters apply"
    );
}

/// CR 106.3 + CR 601.2h: Steel Exemplar cast paying two distinct colors of mana
/// satisfies the unless clause → no +1/+1 counters. REVERT DISCRIMINATOR.
#[test]
fn steel_exemplar_two_colors_spent_suppresses_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let steel = scenario
        .add_creature_to_hand_from_oracle(P0, "Steel Exemplar", 2, 2, STEEL_EXEMPLAR)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::White, ManaCostShard::Blue],
        })
        .id();
    let mut pool = mana(ManaType::White, 1);
    pool.extend(mana(ManaType::Blue, 1));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();

    let outcome = runner.cast(steel).resolve();
    assert_eq!(
        outcome.state().objects[&steel].zone,
        Zone::Battlefield,
        "Steel Exemplar must resolve onto the battlefield"
    );
    assert_eq!(
        outcome.counters(steel, CounterType::Plus1Plus1),
        0,
        "two distinct colors spent satisfies the unless clause → no counters"
    );
}

/// Reach-guard sibling: Steel Exemplar cast paying a single color (one distinct
/// color) does NOT satisfy "two or more colors" → it enters with two +1/+1
/// counters.
#[test]
fn steel_exemplar_one_color_spent_applies_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let steel = scenario
        .add_creature_to_hand_from_oracle(P0, "Steel Exemplar", 2, 2, STEEL_EXEMPLAR)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
        })
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Red, 2));
    let mut runner = scenario.build();

    let outcome = runner.cast(steel).resolve();
    assert_eq!(
        outcome.state().objects[&steel].zone,
        Zone::Battlefield,
        "Steel Exemplar must resolve onto the battlefield"
    );
    assert_eq!(
        outcome.counters(steel, CounterType::Plus1Plus1),
        2,
        "a single distinct color spent leaves the unless clause false → counters apply"
    );
}

/// Read a permanent's counter count directly from state. Reanimation entries do
/// not flow through `CastOutcome`, so these tests inspect the object.
fn counters_on(
    runner: &crate::game::scenario::GameRunner,
    obj: ObjectId,
    kind: CounterType,
) -> u32 {
    runner
        .state()
        .objects
        .get(&obj)
        .and_then(|o| o.counters.get(&kind).copied())
        .unwrap_or(0)
}

/// Put an object onto the battlefield via an effect (reanimation / "put onto the
/// battlefield"), NOT by casting — the entry flows through the replacement
/// pipeline (`object_replacement_candidate_applies`) exactly as a cast entry
/// does, so the "enters with … unless …" self-replacement still applies.
fn reanimate(runner: &mut crate::game::scenario::GameRunner, obj: ObjectId) {
    let mut events = Vec::new();
    crate::game::zone_pipeline::move_object(
        runner.state_mut(),
        crate::game::zone_pipeline::ZoneMoveRequest::effect(obj, Zone::Battlefield, obj),
        &mut events,
    );
}

/// CR 614.1c + CR 109.1: a red spell cast earlier this turn satisfies Hotheaded
/// Giant's unless clause even when the Giant ENTERS BY REANIMATION (a non-cast
/// entry that still routes through `object_replacement_candidate_applies`). The
/// reanimated permanent has no `cast_from_zone` and is not on the stack, so it
/// arms no own-cast exclusion — the foreign red record counts in full → 0
/// counters.
#[test]
fn hotheaded_giant_reanimated_with_prior_red_spell_suppressed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let giant = scenario
        .add_creature_to_graveyard(P0, "Hotheaded Giant", 4, 4)
        .from_oracle_text(HOTHEADED_GIANT)
        .id();
    let mut runner = scenario.build();

    // A DIFFERENT object's red spell was cast this turn.
    runner.state_mut().spells_cast_this_turn_by_player.insert(
        P0,
        crate::im::Vector::from(vec![crate::types::game_state::SpellCastRecord {
            colors: vec![ManaColor::Red],
            spell_object_id: Some(ObjectId(9_999)),
            ..Default::default()
        }]),
    );

    reanimate(&mut runner, giant);
    assert_eq!(
        runner.state().objects[&giant].zone,
        Zone::Battlefield,
        "Giant must reanimate onto the battlefield"
    );
    assert_eq!(
        counters_on(&runner, giant, CounterType::Minus1Minus1),
        0,
        "a prior red spell satisfies the unless clause on a reanimation entry too"
    );
}

/// Reach-guard sibling: with NO red spell cast this turn, the reanimated Giant's
/// own entry is not a cast (no spell-history record), so the unless clause is
/// false → it enters with two -1/-1 counters. Discriminates reanimation from a
/// cast: if the entry were miscounted as a cast the clause would flip.
#[test]
fn hotheaded_giant_reanimated_without_red_spell_applies_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let giant = scenario
        .add_creature_to_graveyard(P0, "Hotheaded Giant", 4, 4)
        .from_oracle_text(HOTHEADED_GIANT)
        .id();
    let mut runner = scenario.build();

    reanimate(&mut runner, giant);
    assert_eq!(
        runner.state().objects[&giant].zone,
        Zone::Battlefield,
        "Giant must reanimate onto the battlefield"
    );
    assert_eq!(
        counters_on(&runner, giant, CounterType::Minus1Minus1),
        2,
        "no red spell cast (reanimation is not a cast) → unless clause false → counters apply"
    );
}

/// CR 614.1c + CR 106.3: a reanimated (un-cast) Steel Exemplar spent no mana, so
/// its `colors_spent_to_cast` is empty and "two or more colors of mana were
/// spent to cast it" is false → it enters with two +1/+1 counters.
#[test]
fn steel_exemplar_reanimated_uncast_applies_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let steel = scenario
        .add_creature_to_graveyard(P0, "Steel Exemplar", 2, 2)
        .from_oracle_text(STEEL_EXEMPLAR)
        .id();
    let mut runner = scenario.build();

    reanimate(&mut runner, steel);
    assert_eq!(
        runner.state().objects[&steel].zone,
        Zone::Battlefield,
        "Steel Exemplar must reanimate onto the battlefield"
    );
    assert_eq!(
        counters_on(&runner, steel, CounterType::Plus1Plus1),
        2,
        "an un-cast entry spent no mana → 'two or more colors' false → counters apply"
    );
}

// --- "number of OTHER spells you've cast this turn" EFFECT-quantity class ---
// Thunder Salvo (DealDamage) and Lock and Load (Draw) share the SAME own-cast
// exclusion seam as the ETB "unless" replacement: their spell-history filter
// carries `FilterProp::Another`, peels through `peel_own_cast_exclusion`, and
// resolves through the `SpellsCastThisTurn` exclusion arm. Unlike an ETB
// entrant, the resolving instant is still on the STACK and carries NO
// `cast_from_zone`, so it is the direct runtime witness for the on-stack arm.

const THUNDER_SALVO: &str = "Thunder Salvo deals X damage to target creature, where X is 2 \
                             plus the number of other spells you've cast this turn.";

/// CR 109.1 + CR 608.2n: cast ALONE, Thunder Salvo's own cast is excluded from
/// "other spells you've cast this turn" → X = 2 + 0 = 2 damage. REVERT
/// DISCRIMINATOR for the on-stack own-cast arm: without it the resolving spell
/// (no `cast_from_zone`) counts its own cast → X = 3.
#[test]
fn thunder_salvo_alone_excludes_own_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P1, "Target Wall", 0, 5).id();
    let salvo = scenario
        .add_spell_to_hand_from_oracle(P0, "Thunder Salvo", true, THUNDER_SALVO)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Red, 1));
    let mut runner = scenario.build();

    let outcome = runner.cast(salvo).target_object(target).resolve();
    assert_eq!(
        outcome.damage_marked(target),
        2,
        "cast alone: own cast excluded → X = 2 + 0"
    );
}

/// Reach-guard sibling: with one OTHER spell already cast this turn (a distinct
/// object), Thunder Salvo deals X = 2 + 1 = 3 — the foreign spell counts and only
/// Thunder Salvo's own cast is excluded.
#[test]
fn thunder_salvo_counts_other_spell_not_own() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P1, "Target Wall", 0, 5).id();
    let salvo = scenario
        .add_spell_to_hand_from_oracle(P0, "Thunder Salvo", true, THUNDER_SALVO)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::Red, 1));
    let mut runner = scenario.build();

    // One OTHER spell was cast this turn (distinct object id).
    runner.state_mut().spells_cast_this_turn_by_player.insert(
        P0,
        crate::im::Vector::from(vec![crate::types::game_state::SpellCastRecord {
            spell_object_id: Some(ObjectId(9_999)),
            ..Default::default()
        }]),
    );

    let outcome = runner.cast(salvo).target_object(target).resolve();
    assert_eq!(
        outcome.damage_marked(target),
        3,
        "one other spell counts; Thunder Salvo's own cast is excluded → X = 2 + 1"
    );
}

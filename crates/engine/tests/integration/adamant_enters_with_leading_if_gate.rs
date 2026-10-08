//! Runtime cast-pipeline coverage for the SENTENCE-INITIAL "if <condition>, "
//! enters-with gate (CR 614.1c), the position Adamant and Spell mastery cards
//! write their gate in.
//!
//! Before the fix, `parse_enters_with_counters` recognized a gate only in the
//! trailing " unless …" / " … if …" positions, so a leading gate was silently
//! swallowed and the replacement became UNCONDITIONAL — the Adamant Paladins
//! entered with a +1/+1 counter no matter what mana paid for them.
//!
//! Built via the `/card-test` recipe: `GameScenario` +
//! `GameRunner::cast(..).resolve()` + `CastOutcome` counter/life deltas, on
//! verbatim Oracle text. Every negative assertion is paired with a positive
//! reach-guard in the same test AND with a structural guard proving the card
//! parsed (a `Some` replacement condition, zero `Effect::Unimplemented`), so an
//! upstream parse failure cannot satisfy it vacuously.
//!
//! REVERT DISCRIMINATOR: `ardenvale_paladin_white_below_threshold_no_counter`
//! (R2). Neutralize `extract_enters_with_leading_if_gate` to always return
//! `NoLeadingIf` and the gate is dropped again — the counter applies
//! unconditionally and R2's `assert_counters(.., 0)` fails.

use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, Effect};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Ardenvale Paladin {3}{W} 2/3 — verbatim Oracle text (`data/card-data.json`).
const ARDENVALE_PALADIN: &str = "Adamant — If at least three white mana was spent to cast this \
                                 spell, this creature enters with a +1/+1 counter on it.";

/// Embereth Paladin {3}{R} 3/1 — verbatim Oracle text, Haste line included.
const EMBERETH_PALADIN: &str = "Haste\nAdamant — If at least three red mana was spent to cast \
                                this spell, this creature enters with a +1/+1 counter on it.";

/// Vantress Paladin {3}{U} 2/2 — full verbatim Oracle text.
const VANTRESS_PALADIN: &str = "Flying\nAdamant — If at least three blue mana was spent to cast \
                                this spell, this creature enters with a +1/+1 counter on it.";

/// Locthwain Paladin {3}{B} 3/2 — the Menace line is dropped because it is
/// irrelevant to this Adamant test and would add an unrelated
/// `Effect::Unimplemented`; the leading-if line under test is verbatim.
const LOCTHWAIN_PALADIN: &str = "Adamant — If at least three black mana was spent to cast this \
                                 spell, this creature enters with a +1/+1 counter on it.";

/// Garenbrig Paladin {4}{G} 4/4 — full verbatim Oracle text.
const GARENBRIG_PALADIN: &str = "Adamant — If at least three green mana was spent to cast this \
                                 spell, this creature enters with a +1/+1 counter on it.\nThis \
                                 creature can't be blocked by creatures with power 2 or less.";

const HENGE_WALKER: &str = "Adamant — If at least three mana of the same color was spent to cast \
                            this spell, this creature enters with a +1/+1 counter on it.";

const RED_AND_BLACK_LEGACY: &str = "If you spent black mana on this creature, it enters with a \
                                    deathtouch counter. If you spent red mana on this creature, it \
                                    enters with a first strike counter. If you spent both, you choose \
                                    which one counter it enters with.\nAt the beginning of your upkeep, \
                                    flip a coin. If it's heads and this creature has deathtouch, or \
                                    it's tails and this creature has haste, create a treasure token.";

/// Dust Animus {1}{W} 1/1 — verbatim Oracle text. The Plot line is dropped
/// because plotting is irrelevant here and its reminder text would only add
/// unrelated parse surface; the leading-if line under test is verbatim.
const DUST_ANIMUS: &str = "Flying\nIf you control five or more untapped lands, this creature \
                           enters with two +1/+1 counters and a lifelink counter on it.";

/// Slaying Fire {2}{R} — verbatim Oracle text. Guards the Adamant ABILITY
/// RIDER, which converges on the same `OfColor` quantity shape as the
/// replacement gate.
const SLAYING_FIRE: &str = "Slaying Fire deals 3 damage to any target.\nAdamant — If at least \
                            three red mana was spent to cast this spell, it deals 4 damage \
                            instead.";

fn mana(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(kind, ObjectId(0), false, vec![]); n]
}

/// Build a pool from a colored/colorless mix, in one place so each row reads as
/// its payment record.
fn pool(colored: &[(ManaType, usize)]) -> Vec<ManaUnit> {
    colored
        .iter()
        .flat_map(|(kind, n)| mana(*kind, *n))
        .collect()
}

/// Cast a Paladin out of an exactly-sized pool and return the outcome plus its
/// object id.
///
/// The pool always holds EXACTLY the mana the cost needs, so the auto-payer
/// has no discretion: every unit in the pool is spent and `colors_spent_to_cast`
/// (CR 601.2h) is fully determined by the pool contents. That removes payment
/// nondeterminism from every assertion below.
fn cast_paladin(
    name: &str,
    oracle: &str,
    generic: u32,
    shard: ManaCostShard,
    power: i32,
    toughness: i32,
    payment: &[(ManaType, usize)],
) -> (CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let paladin = scenario
        .add_creature_to_hand_from_oracle(P0, name, power, toughness, oracle)
        .with_mana_cost(ManaCost::Cost {
            generic,
            shards: vec![shard],
        })
        .id();
    scenario.with_mana_pool(P0, pool(payment));
    let mut runner = scenario.build();

    assert_gate_is_attached(&runner, paladin, name);

    let outcome = runner.cast(paladin).resolve();
    assert_eq!(
        outcome.zone_of(paladin),
        Zone::Battlefield,
        "{name} must resolve onto the battlefield"
    );
    (outcome, paladin)
}

/// Structural reach-guard (the `/card-test` foot-gun #6 defence): the card
/// really parsed, the enters-with replacement really exists, and it really
/// carries a condition. Without this, a "0 counters" assertion would pass just
/// as well on a card whose replacement failed to parse at all.
fn assert_gate_is_attached(runner: &GameRunner, obj: ObjectId, name: &str) {
    let object = &runner.state().objects[&obj];
    assert!(
        !object
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
        "{name} must parse with zero Effect::Unimplemented, got {:?}",
        object.abilities
    );
    let def = object
        .replacement_definitions
        .first()
        .unwrap_or_else(|| panic!("{name} must publish an enters-with replacement"));
    assert!(
        def.condition.is_some(),
        "{name}'s enters-with replacement must carry the leading-if gate; \
         a None condition means the gate was swallowed and the counter is unconditional"
    );
}

fn ability_contains_unimplemented(definition: &AbilityDefinition) -> bool {
    matches!(definition.effect.as_ref(), Effect::Unimplemented { .. })
        || definition
            .sub_ability
            .as_deref()
            .is_some_and(ability_contains_unimplemented)
}

/// Assert that an unsupported enters-with condition stays honest: it must not
/// manufacture a replacement while leaving an explicit parser residual.
fn assert_enters_with_condition_fails_closed(
    name: &str,
    oracle: &str,
    types: &[&str],
    power: i32,
    toughness: i32,
) {
    let types: Vec<String> = types.iter().map(|ty| (*ty).to_owned()).collect();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &[]);
    assert!(
        parsed.replacements.is_empty(),
        "{name} must not publish a replacement for an unsupported condition: {parsed:#?}"
    );
    assert!(
        parsed.abilities.iter().any(ability_contains_unimplemented)
            || parsed
                .triggers
                .iter()
                .filter_map(|trigger| trigger.execute.as_deref())
                .any(ability_contains_unimplemented),
        "{name} must retain at least one Effect::Unimplemented residual: {parsed:#?}"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario
        .add_creature_to_hand_from_oracle(P0, name, power, toughness, oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(creature).resolve();
    assert_eq!(
        outcome.zone_of(creature),
        Zone::Battlefield,
        "{name} must resolve onto the battlefield"
    );
    assert!(
        outcome.state().objects[&creature].counters.is_empty(),
        "{name} must not receive any counters from an unsupported enters-with condition"
    );
}

// ---------------------------------------------------------------------------
// R1-R6 — the Adamant per-color threshold, CR 106.3 + CR 601.2h.
// ---------------------------------------------------------------------------

/// R1 — POSITIVE reach-guard for the whole white family. {W}{W}{W}{W} pays
/// {3}{W}: white spent = 4 >= 3, so the counter applies. Discriminates
/// `OfColor` from `DistinctColors` (which is 1 here, below the threshold).
#[test]
fn ardenvale_paladin_four_white_applies_counter() {
    let (outcome, paladin) = cast_paladin(
        "Ardenvale Paladin",
        ARDENVALE_PALADIN,
        3,
        ManaCostShard::White,
        2,
        3,
        &[(ManaType::White, 4)],
    );
    outcome.assert_counters(paladin, CounterType::Plus1Plus1, 1);
}

/// R2 — **THE PRIMARY REVERT DISCRIMINATOR.** One white + three colorless pays
/// {3}{W}: white spent = 1 < 3, so NO counter. Total mana spent is 4 >= 3, so
/// this row also discriminates `OfColor` from `CastManaSpentMetric::Total`.
///
/// Drop the leading-if peel and the replacement becomes unconditional → 1
/// counter → this assertion fails. Paired reach-guard: R1 above.
#[test]
fn ardenvale_paladin_white_below_threshold_no_counter() {
    let (outcome, paladin) = cast_paladin(
        "Ardenvale Paladin",
        ARDENVALE_PALADIN,
        3,
        ManaCostShard::White,
        2,
        3,
        &[(ManaType::White, 1), (ManaType::Colorless, 3)],
    );
    outcome.assert_counters(paladin, CounterType::Plus1Plus1, 0);
}

/// R6 — pins the comparator (GE, not GT) and the literal threshold 3. Exactly
/// three white → counter applies; exactly two white → it does not.
#[test]
fn ardenvale_paladin_threshold_is_greater_or_equal_three() {
    let (at_threshold, paladin) = cast_paladin(
        "Ardenvale Paladin",
        ARDENVALE_PALADIN,
        3,
        ManaCostShard::White,
        2,
        3,
        &[(ManaType::White, 3), (ManaType::Colorless, 1)],
    );
    at_threshold.assert_counters(paladin, CounterType::Plus1Plus1, 1);

    let (below, paladin) = cast_paladin(
        "Ardenvale Paladin",
        ARDENVALE_PALADIN,
        3,
        ManaCostShard::White,
        2,
        3,
        &[(ManaType::White, 2), (ManaType::Colorless, 2)],
    );
    below.assert_counters(paladin, CounterType::Plus1Plus1, 0);
}

/// R4 — POSITIVE reach-guard for the red family, and proof the color is read
/// per-card rather than hardcoded to the first card fixed (white). Three red +
/// one colorless pays {3}{R}: red spent = 3 >= 3 → counter.
#[test]
fn embereth_paladin_three_red_applies_counter() {
    let (outcome, paladin) = cast_paladin(
        "Embereth Paladin",
        EMBERETH_PALADIN,
        3,
        ManaCostShard::Red,
        3,
        1,
        &[(ManaType::Red, 3), (ManaType::Colorless, 1)],
    );
    outcome.assert_counters(paladin, CounterType::Plus1Plus1, 1);
}

/// R3 — the gate reads the card's OWN color. {W}{W}{W}{R} pays Embereth's
/// {3}{R}: red = 1 < 3 (no counter) even though WHITE = 3 would have passed
/// Ardenvale's gate, and total = 4 would have passed a `Total` gate.
/// Paired reach-guard: R4 above.
#[test]
fn embereth_paladin_reads_red_not_white() {
    let (outcome, paladin) = cast_paladin(
        "Embereth Paladin",
        EMBERETH_PALADIN,
        3,
        ManaCostShard::Red,
        3,
        1,
        &[(ManaType::White, 3), (ManaType::Red, 1)],
    );
    outcome.assert_counters(paladin, CounterType::Plus1Plus1, 0);
}

/// R5 — the row that separates `OfColor` from `DistinctColors` at a point where
/// `DistinctColors` PASSES. {W}{U}{B}{R} pays Embereth's {3}{R}: four distinct
/// colors (>= 3, so a `DistinctColors` gate would fire) but red = 1 < 3, so the
/// correct `OfColor` gate does not. Paired reach-guard: R4 above.
#[test]
fn embereth_paladin_four_distinct_colors_still_no_counter() {
    let (outcome, paladin) = cast_paladin(
        "Embereth Paladin",
        EMBERETH_PALADIN,
        3,
        ManaCostShard::Red,
        3,
        1,
        &[
            (ManaType::White, 1),
            (ManaType::Blue, 1),
            (ManaType::Black, 1),
            (ManaType::Red, 1),
        ],
    );
    outcome.assert_counters(paladin, CounterType::Plus1Plus1, 0);
}

/// Three blue mana satisfies Vantress Paladin's Adamant gate; one blue mana
/// does not. Both pools are exactly {3}{U}, so every staged unit is spent.
#[test]
fn vantress_paladin_blue_threshold() {
    let (at_threshold, paladin) = cast_paladin(
        "Vantress Paladin",
        VANTRESS_PALADIN,
        3,
        ManaCostShard::Blue,
        2,
        2,
        &[(ManaType::Blue, 3), (ManaType::Colorless, 1)],
    );
    at_threshold.assert_counters(paladin, CounterType::Plus1Plus1, 1);

    let (below_threshold, paladin) = cast_paladin(
        "Vantress Paladin",
        VANTRESS_PALADIN,
        3,
        ManaCostShard::Blue,
        2,
        2,
        &[(ManaType::Blue, 1), (ManaType::Colorless, 3)],
    );
    below_threshold.assert_counters(paladin, CounterType::Plus1Plus1, 0);
}

/// Three black mana satisfies Locthwain Paladin's Adamant gate; one black mana
/// does not. Both pools are exactly {3}{B}, so every staged unit is spent.
#[test]
fn locthwain_paladin_black_threshold() {
    let (at_threshold, paladin) = cast_paladin(
        "Locthwain Paladin",
        LOCTHWAIN_PALADIN,
        3,
        ManaCostShard::Black,
        3,
        2,
        &[(ManaType::Black, 3), (ManaType::Colorless, 1)],
    );
    at_threshold.assert_counters(paladin, CounterType::Plus1Plus1, 1);

    let (below_threshold, paladin) = cast_paladin(
        "Locthwain Paladin",
        LOCTHWAIN_PALADIN,
        3,
        ManaCostShard::Black,
        3,
        2,
        &[(ManaType::Black, 1), (ManaType::Colorless, 3)],
    );
    below_threshold.assert_counters(paladin, CounterType::Plus1Plus1, 0);
}

/// Three green mana satisfies Garenbrig Paladin's Adamant gate; one green mana
/// does not. Both pools are exactly {4}{G}, so every staged unit is spent.
#[test]
fn garenbrig_paladin_green_threshold() {
    let (at_threshold, paladin) = cast_paladin(
        "Garenbrig Paladin",
        GARENBRIG_PALADIN,
        4,
        ManaCostShard::Green,
        4,
        4,
        &[(ManaType::Green, 3), (ManaType::Colorless, 2)],
    );
    at_threshold.assert_counters(paladin, CounterType::Plus1Plus1, 1);

    let (below_threshold, paladin) = cast_paladin(
        "Garenbrig Paladin",
        GARENBRIG_PALADIN,
        4,
        ManaCostShard::Green,
        4,
        4,
        &[(ManaType::Green, 1), (ManaType::Colorless, 4)],
    );
    below_threshold.assert_counters(paladin, CounterType::Plus1Plus1, 0);
}

/// Henge Walker's "same color" condition is a distinct max-over-colors metric,
/// not the supported Adamant per-color condition. It must fail closed.
#[test]
fn henge_walker_same_color_adamant_fails_closed() {
    assert_enters_with_condition_fails_closed(
        "Henge Walker",
        HENGE_WALKER,
        &["Artifact", "Creature"],
        2,
        2,
    );
}

/// Red and Black Legacy combines spent-color predicates and a choice between
/// counter payloads; unsupported text must remain visible rather than becoming
/// an unconditional enters-with replacement.
#[test]
fn red_and_black_legacy_enters_with_conditions_fail_closed() {
    assert_enters_with_condition_fails_closed(
        "Red and Black Legacy",
        RED_AND_BLACK_LEGACY,
        &["Creature"],
        2,
        2,
    );
}

// ---------------------------------------------------------------------------
// R7 — Dust Animus: a leading-if gate that is NOT a mana-spent threshold.
// Proves the peel makes the WHOLE `parse_inner_condition` grammar reachable
// from the sentence-initial position, not just the one new arm.
// ---------------------------------------------------------------------------

/// Cast Dust Animus ({1}{W}) out of an exact pool while controlling
/// `untapped_lands` untapped and `tapped_lands` tapped Plains. The lands are
/// never tapped for mana (the pool is pre-staged), so their tap state is
/// controlled purely by the fixture.
fn cast_dust_animus(untapped_lands: usize, tapped_lands: usize) -> (CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let animus = scenario
        .add_creature_to_hand_from_oracle(P0, "Dust Animus", 1, 1, DUST_ANIMUS)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();
    let mut to_tap = Vec::new();
    for _ in 0..untapped_lands {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    for _ in 0..tapped_lands {
        to_tap.push(scenario.add_basic_land(P0, ManaColor::White));
    }
    scenario.with_mana_pool(P0, pool(&[(ManaType::White, 1), (ManaType::Colorless, 1)]));
    let mut runner = scenario.build();
    for land in to_tap {
        runner.state_mut().objects.get_mut(&land).unwrap().tapped = true;
    }

    assert_gate_is_attached(&runner, animus, "Dust Animus");

    let outcome = runner.cast(animus).resolve();
    assert_eq!(
        outcome.zone_of(animus),
        Zone::Battlefield,
        "Dust Animus must resolve onto the battlefield"
    );
    (outcome, animus)
}

/// R7a — POSITIVE reach-guard: five untapped lands satisfies the gate, so Dust
/// Animus keeps both counter payloads. This is the "the peel did not break the
/// already-green card" row (Dust Animus is `supported=true` today, but was
/// silently UNCONDITIONAL — the gate was swallowed).
#[test]
fn dust_animus_five_untapped_lands_applies_counters() {
    let (outcome, animus) = cast_dust_animus(5, 0);
    outcome.assert_counters(animus, CounterType::Plus1Plus1, 2);
    assert_eq!(
        outcome.counters(animus, CounterType::Keyword(KeywordKind::Lifelink)),
        1,
        "the lifelink counter rides the same gated payload"
    );
}

/// R7b — REVERT DISCRIMINATOR (second polarity). Six lands, one of them TAPPED,
/// leaves four untapped: below the "five or more untapped lands" threshold, so
/// no counters. Drop the peel and the payload applies unconditionally → 2 +1/+1
/// counters → this fails. Also discriminates `FilterProp::Untapped` from a bare
/// land count: a count-only reading would see six lands and fire.
/// Paired reach-guard: R7a above.
#[test]
fn dust_animus_only_four_untapped_lands_no_counters() {
    let (outcome, animus) = cast_dust_animus(4, 2);
    outcome.assert_counters(animus, CounterType::Plus1Plus1, 0);
    assert_eq!(
        outcome.counters(animus, CounterType::Keyword(KeywordKind::Lifelink)),
        0,
        "the whole gated payload is suppressed, not just the +1/+1 counters"
    );
}

// ---------------------------------------------------------------------------
// R8 — the Adamant ABILITY RIDER. The same grammar change re-routes 11 riders
// from `AbilityCondition::ManaColorSpent` to the generic
// `QuantityCheck { ManaSpentToCast { .., OfColor } }`. This is the mandatory
// runtime guard on that AST churn: the observable damage must not move.
// ---------------------------------------------------------------------------

fn cast_slaying_fire(payment: &[(ManaType, usize)]) -> CastOutcome {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fire = scenario
        .add_spell_to_hand_from_oracle(P0, "Slaying Fire", true, SLAYING_FIRE)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    scenario.with_mana_pool(P0, pool(payment));
    let mut runner = scenario.build();
    assert!(
        !runner.state().objects[&fire]
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
        "Slaying Fire must parse with zero Effect::Unimplemented, got {:?}",
        runner.state().objects[&fire].abilities
    );
    runner.cast(fire).target_player(P1).resolve()
}

/// R8 positive: three red mana satisfies the Adamant rider → 4 damage, not 3.
#[test]
fn slaying_fire_three_red_deals_four() {
    let outcome = cast_slaying_fire(&[(ManaType::Red, 3)]);
    outcome.assert_life_delta(P1, -4);
}

/// R8 negative (paired with the positive above): one red + two colorless is
/// three TOTAL mana but only one RED, so the rider does not fire → 3 damage.
/// Discriminates `OfColor` from `Total` on the ability-rider path exactly as R2
/// does on the replacement path.
#[test]
fn slaying_fire_one_red_deals_three() {
    let outcome = cast_slaying_fire(&[(ManaType::Red, 1), (ManaType::Colorless, 2)]);
    outcome.assert_life_delta(P1, -3);
}

// ---------------------------------------------------------------------------
// R9 — the Adamant rider whose payload is a CONTINUOUS STATIC GRANT, not an
// ability-level effect. This subclass routes DIFFERENTLY from R8: the gate
// lands on `StaticDefinition.condition` (CR 613 layer 6 keyword grant) rather
// than on the ability's own `condition`, because the payload is a continuous
// effect over a set of permanents.
//
// Why this test exists: the CI parse-diff bot reports Silverflame Ritual's
// ABILITY-level `conditional` as `3+ White spent → ∅`, which reads like a
// dropped gate — the exact bug class this file guards. It is NOT a drop; the
// condition moved down one layer onto the static. The bot's signature reads the
// ability level only, so a layer move renders as `∅`. R8 cannot catch this
// because Slaying Fire's payload is ability-level. Nothing else pins it, so a
// future refactor really COULD drop the static's condition and every existing
// test here would stay green while "creatures you control gain vigilance"
// became unconditional.
// ---------------------------------------------------------------------------

/// Silverflame Ritual {3}{W} — verbatim Oracle text (`data/card-data.json`).
const SILVERFLAME_RITUAL: &str = "Put a +1/+1 counter on each creature you control.\nAdamant — If \
                                  at least three white mana was spent to cast this spell, \
                                  creatures you control gain vigilance until end of turn.";

/// Casts Silverflame Ritual with `payment` and returns the runner plus the
/// controller's creature, so the caller can read granted keywords off the board
/// AFTER resolution (a continuous grant is not visible in `CastOutcome` deltas).
fn cast_silverflame_ritual(payment: &[(ManaType, usize)]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_vanilla(P0, 2, 2);
    let ritual = scenario
        .add_spell_to_hand_from_oracle(P0, "Silverflame Ritual", true, SILVERFLAME_RITUAL)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::White],
        })
        .id();
    scenario.with_mana_pool(P0, pool(payment));
    let mut runner = scenario.build();
    assert!(
        !runner.state().objects[&ritual]
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
        "Silverflame Ritual must parse with zero Effect::Unimplemented, got {:?}",
        runner.state().objects[&ritual].abilities
    );
    runner.cast(ritual).resolve();
    (runner, bear)
}

/// R9 positive reach-guard: three white mana satisfies the rider → the creature
/// gains vigilance. Also proves the UNGATED half of the card resolved (the
/// +1/+1 counter), so a total failure to resolve cannot masquerade as a pass.
#[test]
fn silverflame_ritual_three_white_grants_vigilance() {
    let (runner, bear) = cast_silverflame_ritual(&[(ManaType::White, 3), (ManaType::Colorless, 1)]);
    let obj = &runner.state().objects[&bear];
    assert_eq!(
        obj.counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the ungated first line must always resolve — if this is 0 the spell never resolved \
         and the vigilance assertion below would be vacuous"
    );
    assert!(
        obj.has_keyword(&Keyword::Vigilance),
        "three white mana satisfies the Adamant rider, so the static grant must apply"
    );
}

/// R9 negative (paired with the reach-guard above): one white + three colorless
/// is four TOTAL mana but only one WHITE, so the rider must NOT fire. The
/// +1/+1 counter still lands, proving the spell resolved and the absence of
/// vigilance is a real gate decision rather than a non-resolution.
///
/// This is the assertion that would fail if the static's condition were ever
/// dropped — i.e. if the `∅` the CI bot displays ever became literally true.
#[test]
fn silverflame_ritual_one_white_does_not_grant_vigilance() {
    let (runner, bear) = cast_silverflame_ritual(&[(ManaType::White, 1), (ManaType::Colorless, 3)]);
    let obj = &runner.state().objects[&bear];
    assert_eq!(
        obj.counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the ungated first line must still resolve, so the vigilance check below is not vacuous"
    );
    assert!(
        !obj.has_keyword(&Keyword::Vigilance),
        "only one white mana was spent — the Adamant static grant must stay gated off"
    );
}

/// A mixed-color exact payment still resolves Silverflame Ritual's ungated
/// counter line, but one white mana is insufficient for its white-Adamant
/// vigilance rider.
#[test]
fn silverflame_ritual_one_white_three_blue_does_not_grant_vigilance() {
    let (runner, bear) = cast_silverflame_ritual(&[(ManaType::White, 1), (ManaType::Blue, 3)]);
    let obj = &runner.state().objects[&bear];
    assert_eq!(
        obj.counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the ungated first line must resolve when the generic cost is paid with blue mana"
    );
    assert!(
        !obj.has_keyword(&Keyword::Vigilance),
        "only one white mana was spent — blue mana cannot satisfy the white-Adamant rider"
    );
}

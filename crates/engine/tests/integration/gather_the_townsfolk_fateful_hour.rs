//! CR 111.1 + CR 608.2c-style token-count "instead" override, cross-line ability-word
//! form (CR 614.15): Gather the Townsfolk's "Fateful hour — If you have 5 or
//! less life, create five of those tokens instead."
//!
//! Oracle text (verified against MTGJSON, printings DDQ/DKA/INR/PRM/PW12):
//!   "Create two 1/1 white Human creature tokens.
//!    Fateful hour — If you have 5 or less life, create five of those tokens
//!    instead."
//!
//! "Fateful hour" is a recognized ability word (CR 207.2c) already working on
//! several other cards; the gap this pins is narrower: the second sentence
//! starts with the ability word, so the paragraph-joining loop in
//! `parser::oracle` treats it as its own document item (never merged into the
//! first sentence's body) and only stitches the two back together via
//! `apply_self_replacement_override` (CR 614.15's "a separate ability" case).
//! That fold ran AFTER `oracle_effect::assembly`'s
//! `resolve_those_tokens_anaphors` pass, which only ever saw the override's
//! own one-ability slice (no antecedent), so "create five of those tokens"
//! was left as a raw `Unimplemented` placeholder and the override always
//! swapped in zero tokens instead of five. The fix re-runs
//! `rewrite_those_tokens_from_antecedent` inside `apply_self_replacement_override`
//! once the base and override effects are both in scope.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::Effect;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const GATHER_THE_TOWNSFOLK: &str = "Create two 1/1 white Human creature tokens.\nFateful hour — If you have 5 or less life, create five of those tokens instead.";

fn pool(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(kind, engine::types::identifiers::ObjectId(0), false, vec![]); n]
}

/// Cast Gather the Townsfolk with the controller starting at `life` and
/// return the runner post-resolution.
fn cast_gather_the_townsfolk(life: i32) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, life);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Gather the Townsfolk", false, GATHER_THE_TOWNSFOLK)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        pool(ManaType::White, 1)
            .into_iter()
            .chain(pool(ManaType::Colorless, 1))
            .collect(),
    );
    let mut runner = scenario.build();

    // Reach-guard: the card must parse with zero `Effect::Unimplemented`
    // residue, so a "0 tokens" or "2 tokens" assertion below cannot pass
    // vacuously because the spell failed to parse at all.
    assert!(
        !runner.state().objects[&spell]
            .abilities
            .iter()
            .any(ability_contains_unimplemented),
        "Gather the Townsfolk must parse with zero Effect::Unimplemented, got {:?}",
        runner.state().objects[&spell].abilities
    );

    runner.cast(spell).resolve();
    runner
}

fn ability_contains_unimplemented(def: &engine::types::ability::AbilityDefinition) -> bool {
    matches!(def.effect.as_ref(), Effect::Unimplemented { .. })
        || def
            .sub_ability
            .as_deref()
            .is_some_and(ability_contains_unimplemented)
        || def
            .else_ability
            .as_deref()
            .is_some_and(ability_contains_unimplemented)
}

fn human_token_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|obj| {
                obj.is_token
                    && obj
                        .card_types
                        .subtypes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case("Human"))
            })
        })
        .count()
}

/// Above the Fateful hour threshold (life > 5): the printed default applies —
/// exactly two Human tokens, and the "instead" override must NOT also fire.
#[test]
fn gather_the_townsfolk_creates_two_tokens_above_threshold() {
    let runner = cast_gather_the_townsfolk(20);

    assert_eq!(
        human_token_count(&runner),
        2,
        "with life > 5, Fateful hour's condition is false — the base \
         \"create two\" effect must apply and the override must not fire"
    );
}

/// At the Fateful hour threshold (life == 5, "5 or less"): the override
/// fires and creates five tokens INSTEAD of two — CR 614.6, a replaced event
/// never happens, so the count must be 5, never 2 and never 7.
///
/// Mutation guard: before the fix, `rewrite_those_tokens_from_antecedent`
/// never ran on the cross-line override, so "create five of those tokens"
/// stayed an inert `Unimplemented` placeholder — the swap fired but produced
/// ZERO tokens. This assertion is the one that flips on that regression.
#[test]
fn gather_the_townsfolk_creates_five_tokens_at_threshold() {
    let runner = cast_gather_the_townsfolk(5);

    assert_eq!(
        human_token_count(&runner),
        5,
        "CR 614.6: at 5 life the Fateful hour override REPLACES the base \
         effect — exactly five Human tokens, not two (override did not fire) \
         and not seven (both branches ran)"
    );
}

/// Below the threshold (life < 5) also satisfies "5 or less life" — a second
/// point on the same branch as the exact-threshold test, discriminating a
/// possible off-by-one in the comparator (`<=` vs `<`).
#[test]
fn gather_the_townsfolk_creates_five_tokens_below_threshold() {
    let runner = cast_gather_the_townsfolk(1);

    assert_eq!(
        human_token_count(&runner),
        5,
        "life well below the threshold must still take the \"create five\" \
         branch"
    );
}

//! L02 BB-FU4 — copy-correctness for the resolving-spell "was cast from anywhere
//! other than X" cast-origin condition.
//!
//! BB2 lowered "if this spell was cast from anywhere other than your hand" to a
//! bare `Not(CastFromZone{Hand})`. On a spell COPY that over-fires: a copy has
//! `cast_from_zone == None` (CR 707.10 — a copy of a spell isn't cast; CR 400.7 —
//! a new object has no cast provenance), so `Not(None == Some(Hand)) = Not(false)
//! = TRUE`, wrongly satisfying a "was cast" presupposition for an object that was
//! never cast.
//!
//! BB-FU4 restores the dropped `∃cast` conjunct: the clause now lowers to
//! `And[WasCast{None}, Not(WasCast{Some(Hand)})]` (CR 601.2a), so a copy
//! short-circuits `WasCast{None}` to false. The wrap is applied ONLY to the
//! "anywhere other than X" producer; the opposite-presupposition "you didn't cast
//! it from X" arm stays a bare `Not` (a reanimated/copied object correctly
//! evaluates TRUE there — reanimated Phage still loses).
//!
//! Discriminators (each fails if BB-FU4 is reverted):
//!   1. `antiquities_copy_does_not_grant_counters` — RUNTIME, drives the real
//!      cast/copy/resolve pipeline. Reverting the And-wrap flips the counter sum
//!      from 0 to non-zero (the copy's `PutCounterAll` over-fires).
//!   2. `anywhere_other_than_gains_existential_cast_wrap_via_public_parser` —
//!      public-parser confirmation the "anywhere other than X" rider lowers to
//!      the ∃cast And-wrap. The NARROWING counterpart (the "didn't cast it from
//!      X" arm stays a bare `Not`) is proven directly in the
//!      `oracle_effect::conditions` unit test
//!      `bbfu4_only_anywhere_other_than_gains_existential_cast_conjunct`.
//!   3. `phage_didnt_cast_lowers_to_trigger_condition_not_ability_condition` —
//!      attribution fence: Phage's verbatim clause reaches the (untouched)
//!      `TriggerCondition::WasCast` path, proving the AbilityCondition change
//!      cannot regress it.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{AbilityCondition, TargetRef, TriggerCondition};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim Oracle text (data/card-data.json, 2026-07-12).
const ANTIQUITIES: &str = "Create two 2/2 red and white Spirit creature tokens. \
Then if this spell was cast from anywhere other than your hand, put a +1/+1 counter \
on each Spirit you control.\n\
Flashback {4}{W}{W} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";

// Real Twincast (M19 etc.), verbatim.
const TWINCAST: &str =
    "Copy target instant or sorcery spell. You may choose new targets for the copy.";

// Real Phage the Untouchable, first ability verbatim.
const PHAGE: &str = "When Phage enters, if you didn't cast it from your hand, you lose the game.";

fn parse(oracle: &str, name: &str, types: &[&str]) -> ParsedAbilities {
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(oracle, name, &[], &types, &[])
}

/// First `sub_ability` condition attached anywhere in the parsed abilities.
fn first_sub_condition(parsed: &ParsedAbilities) -> Option<AbilityCondition> {
    parsed
        .abilities
        .iter()
        .filter_map(|a| a.sub_ability.as_ref())
        .find_map(|s| s.condition.clone())
}

/// Per-Spirit +1/+1 counter counts across every P0 battlefield Spirit token.
fn spirit_counters(state: &engine::types::game_state::GameState) -> Vec<u32> {
    state
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.is_token
                && o.controller == P0
                && o.card_types.subtypes.iter().any(|s| s == "Spirit")
        })
        .map(|o| {
            o.counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0)
        })
        .collect()
}

/// Pass priority (resolving the stack) until it is empty, keeping any copy's
/// original targets. Fails loudly on any prompt the copy pipeline is not
/// expected to surface, so a harness surprise never masquerades as a pass.
fn drive_to_empty_stack(runner: &mut GameRunner) {
    for _ in 0..200 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::CopyRetarget { .. } => {
                runner
                    .act(GameAction::KeepAllCopyTargets)
                    .expect("keep the copy's original targets (Antiquities has none)");
            }
            other => panic!("unexpected prompt while resolving the copy: {other:?}"),
        }
    }
    panic!("resolution loop exhausted without emptying the stack");
}

// ===========================================================================
// (1) RUNTIME — the load-bearing copy discriminator.
// ===========================================================================

/// Cast Antiquities on the Loose from HAND, then copy it with Twincast while it
/// is on the stack. The Twincast copy resolves with `cast_from_zone == None`
/// (CR 707.10). With BB-FU4 the copy's gate `And[WasCast{None}, Not(...)]`
/// short-circuits to false → its `PutCounterAll` is skipped → NO Spirit gains a
/// counter. The hand-cast original's gate is also false (cast from hand), so the
/// TOTAL +1/+1 counters across all four Spirit tokens is 0.
///
/// Reach-guard: four Spirit tokens exist (two from the copy + two from the
/// original) — proving the copy resolved and reached the counter-gate dispatch.
///
/// Revert-probe (MEASURED, non-vacuity): restoring the pre-BB-FU4 bare
/// `Not(WasCast{Some(Hand)})` makes the copy's gate `Not(false) = TRUE` → the
/// copy's `PutCounterAll` puts +1/+1 on each Spirit it controls → the counter sum
/// flips from 0 to non-zero. This is the discriminator that isolates the copy
/// path (the hand-cast original never contributes a counter either way).
#[test]
fn antiquities_copy_does_not_grant_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut anti =
        scenario.add_spell_to_hand_from_oracle(P0, "Antiquities on the Loose", false, ANTIQUITIES);
    anti.with_mana_cost(ManaCost::generic(0));
    let antiquities = anti.id();

    let mut tw = scenario.add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST);
    tw.with_mana_cost(ManaCost::generic(0));
    let twincast = tw.id();

    let mut runner = scenario.build();

    // Cast the sorcery from hand (no targets) — it commits to the stack.
    let anti_card = runner.state().objects[&antiquities].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: antiquities,
            card_id: anti_card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Antiquities from hand");
    assert!(
        runner.state().stack.iter().any(|e| e.id == antiquities),
        "reach-guard: Antiquities is on the stack, castable by Twincast"
    );

    // Cast Twincast targeting the Antiquities spell on the stack.
    let tw_card = runner.state().objects[&twincast].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: twincast,
            card_id: tw_card,
            targets: vec![antiquities],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Twincast targeting Antiquities");
    if let WaitingFor::TargetSelection { .. } = runner.state().waiting_for.clone() {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(antiquities)],
            })
            .expect("target Antiquities with Twincast");
    }

    // Resolve Twincast (makes the copy), then the copy, then the original.
    drive_to_empty_stack(&mut runner);

    let counters = spirit_counters(runner.state());
    assert_eq!(
        counters.len(),
        4,
        "reach-guard: two Spirits from the copy + two from the original, got {counters:?}"
    );
    assert_eq!(
        counters.iter().sum::<u32>(),
        0,
        "the copy (cast_from_zone=None) and the hand-cast original both fail the \
         'was cast from anywhere other than your hand' gate → zero +1/+1 counters; \
         got {counters:?}"
    );
}

// ===========================================================================
// (2) PARSE-LEVEL — the narrowing fence.
// ===========================================================================

/// Public-parser confirmation that the "was cast from anywhere other than X"
/// rider lowers to the copy-correct `And[WasCast{None}, Not(WasCast{Some(X)})]`
/// end-to-end (not just at the private stripper). Reverting the And-wrap flips
/// this to a bare `Not`.
///
/// The NARROWING half — that the opposite "you didn't cast it from X" arm stays
/// a BARE `Not` (no wrap) — is proven directly in the
/// `oracle_effect::conditions` unit test
/// `bbfu4_only_anywhere_other_than_gains_existential_cast_conjunct`, which calls
/// `strip_cast_from_zone_conditional` on both arms. It is tested there rather
/// than here because an effect-level "if you didn't cast it from your hand"
/// rider is unreachable through the public sorcery parser (it routes to an
/// `EffectOutcome` optional-decline interpretation), so only the private
/// stripper can exercise that arm in isolation.
#[test]
fn anywhere_other_than_gains_existential_cast_wrap_via_public_parser() {
    let other = parse(
        "Draw a card. If this spell was cast from anywhere other than your hand, draw a card.",
        "Synthetic Anywhere-Other",
        &["Sorcery"],
    );
    assert_eq!(
        first_sub_condition(&other),
        Some(AbilityCondition::And {
            conditions: vec![
                AbilityCondition::WasCast { zone: None },
                AbilityCondition::Not {
                    condition: Box::new(AbilityCondition::WasCast {
                        zone: Some(Zone::Hand)
                    }),
                },
            ],
        }),
        "the 'anywhere other than' producer must gain the ∃cast And-wrap"
    );
}

// ===========================================================================
// (3) PARSE-LEVEL — Phage attribution fence.
// ===========================================================================

/// Phage the Untouchable's "When Phage enters, if you didn't cast it from your
/// hand, you lose the game" reaches the engine via `TriggerCondition::WasCast`
/// (the ETB intervening-if), NOT via any `AbilityCondition`. This proves the
/// BB-FU4 AbilityCondition rename/wrap is provably isolated from the "didn't
/// cast it" TRIGGER family (reanimated Phage still loses). No `sub_ability`
/// `AbilityCondition` is produced for this card.
#[test]
fn phage_didnt_cast_lowers_to_trigger_condition_not_ability_condition() {
    let parsed = parse(PHAGE, "Phage the Untouchable", &["Creature"]);

    let trig = parsed
        .triggers
        .iter()
        .find_map(|t| t.condition.clone())
        .expect("Phage's ETB carries a trigger intervening-if condition");
    assert!(
        matches!(
            &trig,
            TriggerCondition::Not { condition }
                if matches!(condition.as_ref(), TriggerCondition::WasCast { zone: Some(Zone::Hand), .. })
        ),
        "Phage must lower to TriggerCondition::Not(WasCast{{Hand}}), got {trig:?}"
    );
    assert_eq!(
        first_sub_condition(&parsed),
        None,
        "Phage produces NO sub-ability AbilityCondition — the BB-FU4 change cannot touch it"
    );
}

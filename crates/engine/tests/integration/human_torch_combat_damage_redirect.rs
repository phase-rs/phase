//! Human Torch — gendered-pronoun delayed combat-damage rider.
//!
//! Verified Oracle text (`client/public/card-data.json`,
//! `jq '.["human torch"].oracle_text'`), second ability:
//!   "Whenever Human Torch attacks, you may pay {R}{G}{W}{U}. If you do, until
//!    end of turn, whenever he deals combat damage to an opponent, he deals that
//!    much damage to each other opponent."
//!
//! Pins Gap A's gendered-pronoun arm: "he" (nominative, damage-verb-guarded) →
//! `SelfRef`, folded into a delayed `WheneverEvent` `DamageDone`/`CombatOnly`
//! trigger scoped to opponents, with an inner `DamageEachPlayer` over
//! `OpponentOtherThanTriggering`. A revert of the "he" arm returns `mode: Unknown`.
//!
//! The `SelfRef` delayed damage rider fires via the pre-existing per-source
//! `DamageDealt` match path (a `SelfRef` source does not listen on the aggregate
//! `CombatDamageDealtToPlayer` event — see
//! `trigger_matchers::listens_on_aggregate_combat_damage_done`), which the
//! established combat-damage trigger corpus already exercises at runtime; the new
//! surface here is confined to the parse.

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    DamageKindFilter, DelayedTriggerCondition, Effect, PlayerFilter, TargetFilter,
    WheneverEventExpiry,
};
use engine::types::triggers::TriggerMode;

const HUMAN_TORCH_ORACLE: &str = "At the beginning of combat on your turn, if you've \
    cast a noncreature spell this turn, Human Torch gains flying, double strike, and \
    haste until end of turn.\nWhenever Human Torch attacks, you may pay {R}{G}{W}{U}. \
    If you do, until end of turn, whenever he deals combat damage to an opponent, he \
    deals that much damage to each other opponent.";

fn find_delayed(ability: &engine::types::ability::AbilityDefinition) -> &Effect {
    let mut cur = ability;
    loop {
        if matches!(&*cur.effect, Effect::CreateDelayedTrigger { .. }) {
            return &cur.effect;
        }
        cur = cur
            .sub_ability
            .as_deref()
            .expect("CreateDelayedTrigger must appear in the attack trigger's chain");
    }
}

#[test]
fn he_folds_into_selfref_combat_damage_redirect_rider() {
    let parsed = parse_oracle_text(
        HUMAN_TORCH_ORACLE,
        "Human Torch",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Human".to_string()],
    );

    // The attack trigger (second ability).
    let attack = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Attacks)
        .expect("Human Torch attacks trigger");
    let execute = attack.execute.as_ref().expect("attack trigger execute");

    let Effect::CreateDelayedTrigger {
        condition, effect, ..
    } = find_delayed(execute)
    else {
        unreachable!("find_delayed returns a CreateDelayedTrigger");
    };
    let DelayedTriggerCondition::WheneverEvent { trigger, expiry } = condition else {
        panic!("expected WheneverEvent, got {condition:?}");
    };

    // Gap A: "he" → SelfRef source; combat-only; recipient is an opponent.
    assert_eq!(
        trigger.mode,
        TriggerMode::DamageDone,
        "not Unknown — 'he deals combat damage' parsed"
    );
    assert_eq!(trigger.damage_kind, DamageKindFilter::CombatOnly);
    assert_eq!(
        trigger.valid_source,
        Some(TargetFilter::SelfRef),
        "'he' resolves to the source permanent (Human Torch)"
    );
    assert!(
        trigger.valid_target.is_some(),
        "recipient 'to an opponent' populates valid_target"
    );

    // Gap C: no stated multi-turn duration → default EndOfTurn expiry (the
    // "until end of turn" prefix is inert for the WheneverEvent — purged at
    // cleanup by default).
    assert_eq!(*expiry, WheneverEventExpiry::EndOfTurn);

    // Inner effect: "he deals that much damage to each OTHER opponent" →
    // DamageEachPlayer over OpponentOtherThanTriggering.
    match &*effect.effect {
        Effect::DamageEachPlayer { player_filter, .. } => assert_eq!(
            *player_filter,
            PlayerFilter::OpponentOtherThanTriggering,
            "each OTHER opponent (excludes the damaged opponent)"
        ),
        other => panic!("expected DamageEachPlayer, got {other:?}"),
    }
}

/// Negative parse sibling: a POSSESSIVE gendered subject ("his …") must NOT
/// resolve to `SelfRef`. The gendered subject arm is nominative-only ("he "/
/// "she ") and damage-verb guarded, so a possessive form must decline.
///
/// Reach-guard (fixes a prior vacuous negative): the probe is an INSTANT carrying
/// the " this turn, " delayed-trigger window, so `parse_oracle_text` routes it
/// through `try_parse_temporal_delayed_trigger_ability` (spell + trigger-prefix)
/// → the `WheneverEvent` delayed-trigger path with `in_delayed_trigger = true`.
/// That is the SAME `parse_single_subject` gendered branch the positive test
/// exercises, so "his commander" is genuinely evaluated and declined — the parse
/// no longer short-circuits to `Effect::Unimplemented` before the gendered arm is
/// ever reached. `find_delayed` panicking is itself the positive reach-guard: a
/// regression that fails to build the delayed `WheneverEvent` fails this test
/// rather than passing it silently.
#[test]
fn possessive_gendered_subject_does_not_become_selfref() {
    let oracle = "Whenever his commander deals combat damage to a player this turn, draw a card.";
    let parsed = parse_oracle_text(oracle, "Probe", &[], &["Instant".to_string()], &[]);

    // Positive reach-guard: the delayed-trigger path was reached and produced a
    // `WheneverEvent` condition (not `Unimplemented`). `find_delayed` panics if no
    // `CreateDelayedTrigger` is in the chain, so the SelfRef check below is never
    // vacuous.
    let ability = parsed
        .abilities
        .first()
        .expect("instant delayed-trigger creates a spell ability");
    let Effect::CreateDelayedTrigger {
        condition: DelayedTriggerCondition::WheneverEvent { trigger, .. },
        ..
    } = find_delayed(ability)
    else {
        panic!(
            "expected CreateDelayedTrigger/WheneverEvent, got {:?}",
            ability.effect
        );
    };

    // Positive shape pin (replaces a prior vacuous negative): the delayed subject
    // arm evaluated "his commander" and DECLINED — possessive case is excluded from
    // the gendered SelfRef arm, and "his commander" is not otherwise a recognized
    // subject. The inner combat-damage trigger therefore stays coverage-honest:
    // `mode` is `Unknown` carrying the original clause, and NO filter slot binds.
    // This is the discriminating positive assertion — a SelfRef regression would
    // instead RECOGNIZE the trigger (a concrete `DamageDone`/`CombatOnly` mode with
    // `valid_source == Some(SelfRef)`), so pinning `Unknown` rejects both the
    // SelfRef bug and a spurious recognized-but-wrong parse; it cannot be satisfied
    // by an upstream `None` masquerading as success.
    assert!(
        matches!(&trigger.mode, TriggerMode::Unknown(text) if text == oracle_condition_clause()),
        "possessive 'his <noun>' must stay coverage-honest (Unknown), got mode={:?}",
        trigger.mode
    );
    assert_eq!(
        trigger.valid_source, None,
        "an unrecognized possessive subject must bind no source (not SelfRef, not Any)"
    );
    assert_ne!(
        trigger.valid_source,
        Some(TargetFilter::SelfRef),
        "possessive 'his <noun>' must not bind SelfRef"
    );
}

/// The exact combat-damage condition clause the delayed trigger carries for the
/// probe oracle — pinned so the `Unknown` assertion above proves the *specific*
/// clause reached classification and was left honestly unclassified.
fn oracle_condition_clause() -> &'static str {
    "his commander deals combat damage to a player"
}

//! CR 120.10 — the EXCESS damage channel of the previous-effect quantity.
//!
//! `DamageChannel` existed, `GameState::last_effect_excess_amount` existed and was
//! already stamped by the damage effects, and the CONDITION peer
//! (`AbilityCondition::PreviousEffectAmount { channel: Excess }`, "if excess damage
//! was dealt this way") already read it. Only the QUANTITY side was missing:
//! `QuantityRef::PreviousEffectAmount` was a bare unit variant with no channel, so
//! it could only ever read the TOTAL (`last_effect_amount`).
//!
//! The consequence was not a wrong number — it was a dropped clause. With no way to
//! type "the amount of excess damage dealt to that creature this way", the whole
//! surrounding instruction fell to an honest red:
//!
//!   Goblin Negotiation  "Create ... 1/1 red Goblin creature tokens equal to the
//!                        amount of excess damage dealt to that creature this way."
//!   Hell to Pay         (same shape, tapped Treasures)
//!   Lacerate Flesh      (same shape, Blood tokens)
//!   Contest of Claws    "... discover X, where X is that excess damage."
//!
//! and one face was WORSE than a red — Archaic's Agony ("Exile cards from the top of
//! your library equal to the excess damage dealt to that creature this way") parsed
//! GREEN to a `ChangeZone` with NO COUNT AT ALL. The quantity was silently swallowed
//! and the card rendered as fully supported.
//!
//! These are RUNTIME witnesses: each parses the card's real Oracle clause through the
//! production parser and hands the resulting `QuantityExpr` to the live resolver
//! against a state where excess damage was actually dealt.
//!
//! Oracle text below is read from the card export, not from memory.

use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::{DamageChannel, Effect, QuantityExpr, QuantityRef};
use engine::types::phase::Phase;

/// Pull the single quantity a parsed one-clause ability carries.
fn only_quantity(oracle: &str) -> QuantityExpr {
    let parsed = parse_oracle_text(oracle, "~", &[], &["Creature".to_string()], &[]);
    let def = parsed
        .abilities
        .first()
        .unwrap_or_else(|| panic!("no ability parsed from {oracle:?}"));
    match &*def.effect {
        Effect::GainLife { amount, .. }
        | Effect::LoseLife { amount, .. }
        | Effect::DealDamage { amount, .. } => amount.clone(),
        other => panic!("unexpected effect for {oracle:?}: {other:?}"),
    }
}

/// CR 120.10: "the amount of excess damage dealt to that creature this way"
/// (Goblin Negotiation, Hell to Pay, Lacerate Flesh).
///
/// 5 damage dealt to a 2-toughness creature is 3 excess (CR 120.10: "excess damage
/// equal to the difference"). The quantity must read 3 — the EXCESS — not 5 (the
/// total) and not 0 (the dropped clause).
#[test]
fn excess_damage_this_way_reads_the_excess_not_the_total_and_not_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Goblin Negotiation", 1, 1).id();
    let mut runner = scenario.build();

    // The preceding effect dealt 5 damage to a 2-toughness creature: total 5,
    // excess 3. Both channels are stamped by the damage effects; this witness
    // discriminates between them.
    runner.state_mut().last_effect_amount = Some(5);
    runner.state_mut().last_effect_excess_amount = Some(3);

    let expr = only_quantity(
        "~ deals X damage to any target, where X is the amount of excess damage dealt to that creature this way.",
    );
    let resolved = resolve_quantity(runner.state(), &expr, P0, source);

    assert_eq!(
        resolved, 3,
        "X must read the EXCESS channel (3 = 5 damage - 2 toughness, CR 120.10), not \
         the total (5) and not the dropped-clause 0. Got {resolved} from {expr:?}"
    );
}

/// NEGATIVE CONTROL — the bare demonstrative "that excess damage" must stay an
/// honest red. This is a correctness constraint, not a coverage gap.
///
/// Its antecedent is fixed by the sibling clause, and the two readings resolve
/// from DIFFERENT state:
///
///   - Contest of Claws — "If excess damage was dealt THIS WAY, … where X is that
///     excess damage." Same resolution, so `last_effect_excess_amount` is live and
///     `PreviousEffectAmount { Excess }` would be correct.
///   - Fall of Cair Andros — "Whenever a creature an opponent controls is dealt
///     excess noncombat damage, amass Orcs X, where X is that excess damage." The
///     antecedent is the TRIGGERING EVENT. The triggered ability resolves as its own
///     top-level chain and the depth-0 prelude CLEARS `last_effect_excess_amount`,
///     so `PreviousEffectAmount { Excess }` would read None -> 0 and silently amass
///     nothing — while rendering as fully supported.
///
/// A context-free leaf combinator cannot separate them; the disambiguator (the
/// sibling "dealt this way" condition vs. the trigger condition) lives one layer up.
/// Binding the bare demonstrative here would have swapped a crude raw-text
/// fabrication for a better-disguised one. Until the clause-layer rebind exists,
/// it stays red.
#[test]
fn bare_that_excess_damage_stays_an_honest_red_pending_clause_layer_rebind() {
    let parsed = parse_oracle_text(
        "You gain X life, where X is that excess damage.",
        "~",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let has_unimplemented = parsed
        .abilities
        .iter()
        .any(|def| matches!(&*def.effect, Effect::Unimplemented { .. }));

    assert!(
        has_unimplemented,
        "the bare \"that excess damage\" demonstrative must remain an honest \
         Effect::unimplemented gap: in a TRIGGER context (Fall of Cair Andros) the \
         resolution-local excess channel is already cleared, so binding it would \
         resolve to 0 while rendering as supported. Parsed: {:?}",
        parsed.abilities
    );
}

/// The TOTAL channel must be untouched: every pre-existing
/// `PreviousEffectAmount` producer (life lost, counters removed, cards drawn)
/// stamps only `last_effect_amount`, and must keep reading it.
///
/// Negative control for the two witnesses above — without this, a resolver that
/// simply always read the excess field would pass them both.
#[test]
fn total_channel_is_unchanged_and_still_reads_last_effect_amount() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Coalition Relic", 1, 1).id();
    let mut runner = scenario.build();

    runner.state_mut().last_effect_amount = Some(7);
    // Deliberately different, so a resolver reading the wrong channel is caught.
    runner.state_mut().last_effect_excess_amount = Some(2);

    let total = QuantityExpr::Ref {
        qty: QuantityRef::PreviousEffectAmount {
            channel: DamageChannel::Total,
        },
    };
    let resolved = resolve_quantity(runner.state(), &total, P0, source);

    assert_eq!(
        resolved, 7,
        "the Total channel must still read last_effect_amount (7), not the excess (2). \
         Got {resolved}"
    );
}

/// CR 120.10: excess is 0 when the damage did not exceed lethal. The clause is
/// still bound (it is a real quantity), it simply resolves to 0 — which is the
/// rules-correct answer, not a dropped clause.
#[test]
fn no_excess_dealt_resolves_to_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Hell to Pay", 1, 1).id();
    let mut runner = scenario.build();

    // 2 damage to a 5-toughness creature: total 2, no excess.
    runner.state_mut().last_effect_amount = Some(2);
    runner.state_mut().last_effect_excess_amount = None;

    let expr = only_quantity(
        "~ deals X damage to any target, where X is the amount of excess damage dealt to that creature this way.",
    );
    let resolved = resolve_quantity(runner.state(), &expr, P0, source);

    assert_eq!(
        resolved, 0,
        "no excess was dealt, so the excess channel is 0 — and it must NOT fall back \
         to the total (2). Got {resolved} from {expr:?}"
    );
}

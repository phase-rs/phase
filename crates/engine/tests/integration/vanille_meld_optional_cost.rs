//! Meld-gate parser regression for Vanille, Cheerful l'Cie / Titania, Voice of
//! Gaea (FIN + shared meld class). These drive the real production parser entry
//! (`parse_oracle_text` → normalize/self-ref mask → `parse_meld_gate` → clause
//! dispatch → lower), so each assertion below flips when a fix is reverted:
//!
//!   * `parse_meld_gate` rewrite → Vanille's optional-cost gate and Titania's
//!     leading-condition gate stop parsing → their meld sub-clauses lower to
//!     `Effect::Unimplemented` (fails `vanille_optional_cost_gate_lowers_to_meld`
//!     and `titania_leading_condition_gate_lowers_to_meld` via
//!     `assert_no_unimplemented`).
//!   * chunk-ctx `pending_meld_partner` thread + per-clause dispatch site → the
//!     Vanille reflexive "If you do, exile them, then meld them into R" sub-clause
//!     lowers to `Effect::Unimplemented` (fails
//!     `vanille_optional_cost_gate_lowers_to_meld`, which also pins the
//!     `PayCost → OptionalEffectPerformed`-gated `Meld` structure).
//!   * FIX-3 self-ref mask arm → Titania's shared pre-comma token folds the meld
//!     RESULT to "~, Gaea Incarnate" (fails
//!     `titania_leading_condition_gate_lowers_to_meld`). The land-count
//!     intervening-if is pinned by `titania_condition_is_three_conjunct_and_with_land_count`;
//!     the direct triggered Gisela form is left untouched
//!     (`gisela_direct_meld_unchanged`). The end-to-end runtime accept/decline
//!     pay-gate proof lives in `game::meld_tests`
//!     (`vanille_optional_pay_gates_meld_accept_vs_decline`).

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, Comparator, CountScope, Effect, EffectOutcomeSignal,
    QuantityExpr, QuantityRef, TriggerCondition, TriggerDefinition, TypeFilter, ZoneRef,
};

// Verbatim Scryfall Oracle text (checked 2026-07 via api.scryfall.com).
const VANILLE: &str = "When Vanille enters, mill two cards, then return a permanent card from your graveyard to your hand.\nAt the beginning of your first main phase, if you both own and control Vanille and a creature named Fang, Fearless l'Cie, you may pay {3}{B}{G}. If you do, exile them, then meld them into Ragnarok, Divine Deliverance.";

const TITANIA: &str = "Reach\nWhenever one or more land cards are put into your graveyard from anywhere, you gain 2 life.\nAt the beginning of your upkeep, if there are four or more land cards in your graveyard and you both own and control Titania, Voice of Gaea and a land named Argoth, Sanctum of Nature, exile them, then meld them into Titania, Gaea Incarnate.";

const GISELA: &str = "Flying, first strike, lifelink\nAt the beginning of your end step, if you both own and control Gisela and a creature named Bruna, the Fading Light, exile them, then meld them into Brisela, Voice of Nightmares.";

/// Flatten the `AbilityDefinition` tree (effect + sub/else/mode branches) so a
/// meld card's `PayCost → Meld` reflexive chain is fully visited.
fn walk<'a>(def: &'a AbilityDefinition, out: &mut Vec<&'a AbilityDefinition>) {
    out.push(def);
    if let Some(s) = &def.sub_ability {
        walk(s, out);
    }
    if let Some(e) = &def.else_ability {
        walk(e, out);
    }
    for m in &def.mode_abilities {
        walk(m, out);
    }
}

fn defs(trigger: &TriggerDefinition) -> Vec<&AbilityDefinition> {
    let mut out = Vec::new();
    if let Some(execute) = &trigger.execute {
        walk(execute, &mut out);
    }
    out
}

fn meld_of(trigger: &TriggerDefinition) -> (String, String, String) {
    defs(trigger)
        .into_iter()
        .find_map(|d| match d.effect.as_ref() {
            Effect::Meld {
                source,
                partner,
                result,
                ..
            } => Some((source.clone(), partner.clone(), result.clone())),
            _ => None,
        })
        .expect("the meld trigger lowers to an Effect::Meld")
}

/// The trigger whose execute tree carries the `Effect::Meld` (robust against
/// trigger ordering / mode — both cards also carry an unrelated trigger).
fn meld_trigger(oracle: &str, name: &str, types: &[&str]) -> TriggerDefinition {
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &[]);
    parsed
        .triggers
        .into_iter()
        .find(|t| {
            t.execute
                .as_ref()
                .map(|e| {
                    let mut v = Vec::new();
                    walk(e, &mut v);
                    v.iter()
                        .any(|d| matches!(d.effect.as_ref(), Effect::Meld { .. }))
                })
                .unwrap_or(false)
        })
        .expect("a meld trigger is produced")
}

/// `Effect::Unimplemented` is the honest parse-gap marker; it serializes with the
/// `#[serde(tag = "type")]` discriminator `"Unimplemented"`. A single JSON scan
/// catches it anywhere in the tree, including effect-nested payloads the
/// `AbilityDefinition` walker would skip.
fn assert_no_unimplemented(trigger: &TriggerDefinition, label: &str) {
    let json = serde_json::to_string(trigger).expect("serialize trigger");
    assert!(
        !json.contains("\"type\":\"Unimplemented\""),
        "{label}: meld trigger still contains an Unimplemented part: {json}"
    );
}

#[test]
fn vanille_optional_cost_gate_lowers_to_meld() {
    let trigger = meld_trigger(VANILLE, "Vanille, Cheerful l'Cie", &["Creature"]);
    assert_no_unimplemented(&trigger, "Vanille");

    // CR 118.12: the reflexive "you may pay {3}{B}{G}" is an optional resolution
    // cost. The trigger's execute IS that pay — a `PayCost` on an `optional`
    // ability. Reverting the `parse_meld_gate` rewrite collapses this to
    // Unimplemented (already covered by `assert_no_unimplemented`).
    let execute = trigger.execute.as_deref().expect("Vanille has an execute");
    assert!(
        matches!(execute.effect.as_ref(), Effect::PayCost { .. }),
        "the execute root is the {{3}}{{B}}{{G}} pay, got {:?}",
        execute.effect
    );
    assert!(
        execute.optional,
        "the {{3}}{{B}}{{G}} pay is optional (you may pay)"
    );

    // CR 118.12 STRUCTURE: declining the pay must NOT meld. Prove the meld is the
    // `OptionalEffectPerformed`-gated CHILD of the PayCost — not a sibling that
    // would fire on decline. Bind to the exact parsed field path
    // (execute.sub_ability = { Meld, condition: EffectOutcome{OptionalEffectPerformed} });
    // a mis-gated sibling meld fails this assertion.
    let gated = execute
        .sub_ability
        .as_deref()
        .expect("the PayCost stages a gated sub_ability");
    let Effect::Meld {
        source,
        partner,
        result,
        ..
    } = gated.effect.as_ref()
    else {
        panic!(
            "the PayCost's sub_ability must be the meld, got {:?}",
            gated.effect
        );
    };
    assert_eq!(source, "Vanille, Cheerful l'Cie");
    assert_eq!(partner, "Fang, Fearless l'Cie");
    assert_eq!(result, "Ragnarok, Divine Deliverance");
    assert!(
        !result.contains('~'),
        "result must not carry a self-ref token"
    );
    assert_eq!(
        gated.condition,
        Some(AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        }),
        "the meld is gated on OptionalEffectPerformed — declining the pay must not meld"
    );
}

#[test]
fn titania_leading_condition_gate_lowers_to_meld() {
    let trigger = meld_trigger(TITANIA, "Titania, Voice of Gaea", &["Creature"]);
    assert_no_unimplemented(&trigger, "Titania");

    let (source, partner, result) = meld_of(&trigger);
    assert_eq!(source, "Titania, Voice of Gaea");
    assert_eq!(partner, "Argoth, Sanctum of Nature");
    // FIX-3 mask discriminator: without the "meld them into " mask arm the shared
    // pre-comma token "Titania" folds → "~, Gaea Incarnate".
    assert_eq!(result, "Titania, Gaea Incarnate");
}

/// CR 603.4: Titania's intervening-if is a 3-conjunct `And` — the leading
/// graveyard-land count PLUS the two own/control conjuncts. Dropping the leading
/// prepend (or failing to parse it) yields a 2-conjunct `And`.
#[test]
fn titania_condition_is_three_conjunct_and_with_land_count() {
    let trigger = meld_trigger(TITANIA, "Titania, Voice of Gaea", &["Creature"]);
    let Some(TriggerCondition::And { conditions }) = trigger.condition.as_ref() else {
        panic!(
            "expected a 3-conjunct And condition, got {:?}",
            trigger.condition
        );
    };
    assert_eq!(conditions.len(), 3, "leading land-count + self + partner");

    // First conjunct: "four or more land cards in your graveyard" GE 4.
    match &conditions[0] {
        TriggerCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::ZoneCardCount {
                            zone: ZoneRef::Graveyard,
                            card_types,
                            scope: CountScope::Controller,
                            filter: None,
                        },
                },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 4 },
        } => assert_eq!(card_types, &vec![TypeFilter::Land]),
        other => panic!("first conjunct must be a graveyard land-count GE 4, got {other:?}"),
    }
    // The remaining two are own/control conjuncts.
    assert_eq!(
        conditions
            .iter()
            .filter(|c| matches!(c, TriggerCondition::ControlCount { minimum: 1, .. }))
            .count(),
        2,
        "self + partner own/control conjuncts"
    );
}

/// Regression: the pre-existing direct triggered meld (Gisela) is untouched — a
/// 2-conjunct gate, clean parse, uncorrupted result (Brisela shares no token with
/// Gisela, so the new mask arm is a no-op for it).
#[test]
fn gisela_direct_meld_unchanged() {
    let trigger = meld_trigger(GISELA, "Gisela, the Broken Blade", &["Creature"]);
    assert_no_unimplemented(&trigger, "Gisela");

    let (_source, partner, result) = meld_of(&trigger);
    assert_eq!(partner, "Bruna, the Fading Light");
    assert_eq!(result, "Brisela, Voice of Nightmares");

    let Some(TriggerCondition::And { conditions }) = trigger.condition.as_ref() else {
        panic!("expected a 2-conjunct And, got {:?}", trigger.condition);
    };
    assert_eq!(conditions.len(), 2, "self + partner, no leading condition");
}

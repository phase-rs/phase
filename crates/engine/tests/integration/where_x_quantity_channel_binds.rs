//! CR 107.3c — the QUANTITY channel of "where X is …" must resolve to the real
//! value, not 0.
//!
//! Sibling of `bishop_of_binding_where_x_exiled_card_power` (harvest #48, which
//! covered the P/T channel). When a "where X is <expr>" clause defined X with an
//! expression the parser could not type, the quantity channel fabricated
//! `QuantityRef::Variable { name: "<raw oracle text>" }` and carried on. That
//! node is well-typed and renders as a supported dynamic quantity in the
//! coverage report, but `game/quantity.rs` dispatches the non-`"X"` `Variable`
//! arm through `state.last_named_choice` and `.unwrap_or(0)` — so the quantity
//! read 0 (or, worse, an unrelated number left behind by some earlier "choose a
//! number"). 93 faces / 74 distinct expressions carried such a node pool-wide.
//!
//! These are RUNTIME witnesses, not AST-shape assertions: each one parses the
//! card's real Oracle clause through the production parser, then hands the
//! resulting `QuantityExpr` to the live resolver (`game::quantity::resolve_quantity`)
//! against a game state where the referenced value is actually set. If the
//! binding is dropped, the resolver returns 0 — which is exactly what each
//! assertion discriminates against.
//!
//! Oracle text below is read from the card export, not from memory.

use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::{ChosenAttribute, Effect, QuantityExpr};
use engine::types::phase::Phase;

/// Pull the single quantity a parsed one-clause ability carries.
///
/// Deliberately matches only the effects these witnesses use, so a parse that
/// silently lowers to something else (an `Unimplemented` gap, say) fails loudly
/// here instead of being skipped.
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
        Effect::Scry { count, .. } | Effect::Draw { count, .. } => count.clone(),
        other => panic!("unexpected effect for {oracle:?}: {other:?}"),
    }
}

/// Alchemy intensity (Arek, False Goldwarden; Legion's Chant; Mycelic Ballad).
/// `QuantityRef::Intensity { scope: Source }` reads the source object's live
/// intensity counter. Pre-fix this was `Variable("~'s intensity")` → 0, so Arek
/// drained for nothing.
#[test]
fn where_x_intensity_resolves_to_the_sources_intensity_not_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Arek, False Goldwarden", 2, 2)
        .id();
    let mut runner = scenario.build();

    // Starting intensity 0, intensified three times.
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("source on battlefield")
        .intensity = 3;

    let expr = only_quantity("Target opponent loses X life, where X is ~'s intensity.");
    let resolved = resolve_quantity(runner.state(), &expr, P0, source);

    assert_eq!(
        resolved, 3,
        "X must resolve to the source's intensity (3). A raw-text \
         QuantityRef::Variable resolves to 0 — a silent no-op that still reads as \
         supported in the coverage report. Got {resolved} from {expr:?}"
    );
}

/// "the chosen number" (Liquid Fire; Fluros of Myra's Marvels).
/// `QuantityRef::ChosenNumber` reads `ChosenAttribute::Number` off the source.
#[test]
fn where_x_chosen_number_resolves_to_the_chosen_number_not_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Liquid Fire", 1, 1).id();
    let mut runner = scenario.build();

    // "As an additional cost to cast this spell, choose a number between 0 and 5."
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("source on battlefield")
        .chosen_attributes = vec![ChosenAttribute::Number(4)];

    let expr = only_quantity("~ deals X damage to any target, where X is the chosen number.");
    let resolved = resolve_quantity(runner.state(), &expr, P0, source);

    assert_eq!(
        resolved, 4,
        "X must resolve to the number the player actually chose (4), not 0. \
         Got {resolved} from {expr:?}"
    );
}

/// "the amount of mana spent to cast her" (Toph, Greatest Earthbender).
/// The gendered self-anaphora is the same self-object axis as "it"/"them"
/// (CR 400.7d) — it was simply missing from the subject list, so the clause fell
/// through to a raw-text placeholder and Toph earthbent 0.
#[test]
fn where_x_mana_spent_to_cast_resolves_to_the_mana_paid_not_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Toph, Greatest Earthbender", 3, 3)
        .id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("source on battlefield")
        .mana_spent_to_cast_amount = 5;

    for oracle in [
        "You gain X life, where X is the amount of mana spent to cast her.",
        "You gain X life, where X is the amount of mana spent to cast it.",
        "You gain X life, where X is the amount of mana spent to cast this spell.",
    ] {
        let expr = only_quantity(oracle);
        let resolved = resolve_quantity(runner.state(), &expr, P0, source);
        assert_eq!(
            resolved, 5,
            "X must resolve to the mana actually paid (5) for {oracle:?}, not 0. \
             Got {resolved} from {expr:?}"
        );
    }
}

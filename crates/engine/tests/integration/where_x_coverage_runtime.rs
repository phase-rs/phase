//! CR 107.3c — RUNTIME witnesses for the where-X coverage enumeration (task #95).
//!
//! The parse-level pins in `where_x_totality_guard` prove the gap node is gone. They do
//! NOT prove the bound quantity resolves to the right NUMBER — a slot could bind to a
//! plausible-looking `QuantityRef` that reads the wrong state and still show up green on
//! every ledger. These witnesses close that hole: each parses the card's real Oracle text
//! through the production parser and hands the resulting quantity to the LIVE resolver
//! against a state where the referenced fact is actually true.
//!
//! The failure they discriminate is specifically ZERO. Before #95 these slots kept a bare
//! `QuantityRef::Variable { name: "X" }`, which resolves to 0 — an 0/0 token, a
//! monstrosity that adds no counters. So each assertion is written against a state whose
//! correct answer is a NON-zero number that is also not the fixed value of any sibling
//! slot, so neither "still unbound" nor "bound to the wrong reference" can pass.
//!
//! Oracle text is verbatim from MTGJSON.

use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, PtValue, QuantityExpr};
use engine::types::phase::Phase;

/// CR 613.4b — Tivash, Gloom Summoner. "create an X/X black Demon creature token with
/// flying, where X is the amount of life you gained this turn."
///
/// The token's P/T reached the where-X rewriter as `PtValue::Quantity(Ref { Variable("X") })`
/// rather than `PtValue::Variable("X")`, so the old matcher passed it through untouched and
/// the Demon entered as an **0/0** — dead on arrival to state-based actions (CR 704.5f)
/// while the card rendered as fully supported.
///
/// The witness gains 5 life and asserts the token's power resolves to 5. A still-unbound
/// slot resolves to 0; the token's `count` slot is a fixed 1, so a mis-bind onto the wrong
/// slot would read 1. Only a correct bind reads 5.
#[test]
fn tivash_x_x_demon_token_reads_life_gained_not_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Tivash, Gloom Summoner", 3, 3)
        .id();
    let mut runner = scenario.build();

    // The trigger's intervening-if is "if you gained life this turn"; the quantity reads
    // the same tracked total.
    runner.state_mut().players[0].life_gained_this_turn = 5;

    let power = tivash_token_power();
    let resolved = resolve_quantity(runner.state(), &power, P0, source);

    assert_eq!(
        resolved, 5,
        "CR 613.4b: the Demon's X/X must read the life gained this turn (5). 0 means the P/T slot \
         is still an unbound Variable(\"X\") and the token enters 0/0, dying immediately to SBAs \
         (CR 704.5f); 1 means it bound to the token's `count` slot instead. Got {resolved} from \
         {power:?}"
    );
}

/// Pull the X/X token's power out of Tivash's real Oracle text.
///
/// Reaching through the sub-ability ("If you do, create …") is deliberate: that is exactly
/// where the unbound placeholder used to survive, so a helper that could not find the token
/// there would make the witness vacuous.
fn tivash_token_power() -> QuantityExpr {
    let parsed = parse_oracle_text(
        "Lifelink\nAt the beginning of your end step, if you gained life this turn, you may pay X \
         life, where X is the amount of life you gained this turn. If you do, create an X/X black \
         Demon creature token with flying.",
        "Tivash, Gloom Summoner",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .first()
        .expect("Tivash's end-step trigger must parse");
    let sub = trigger
        .execute
        .as_ref()
        .expect("the trigger must lower an execute body")
        .sub_ability
        .as_ref()
        .expect("the 'If you do, create …' continuation must lower to a sub-ability");
    match &*sub.effect {
        Effect::Token { power, .. } => match power {
            PtValue::Quantity(quantity) => quantity.clone(),
            other => panic!(
                "the Demon's power must lower to a dynamic quantity, not {other:?} — if this is \
                 still PtValue::Variable(\"X\") the where-X bind never ran"
            ),
        },
        other => panic!("expected the sub-ability to create a Token, got {other:?}"),
    }
}

/// CR 122.1 — Maester Seymour. "Monstrosity X, where X is the number of counters among
/// creatures you control."
///
/// `Effect::Monstrosity` was one of the unenumerated `QuantityExpr` carriers, so its count
/// kept a bare `Variable("X")` and the ability became monstrous while adding **zero**
/// +1/+1 counters — a full no-op under a green badge.
///
/// The board is stacked so the right answer (7) is distinguishable from every wrong one:
/// counters are split 4 + 3 across two of the controller's creatures (so a bind that reads
/// only one object would read 4 or 3), and an opponent's creature carries 2 more (so a bind
/// that ignores the "you control" filter would read 9).
#[test]
fn maester_seymour_monstrosity_counts_counters_among_your_creatures_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Maester Seymour", 4, 4)
        .with_plus_counters(4)
        .id();
    scenario
        .add_creature(P0, "Bear", 2, 2)
        .with_plus_counters(3);
    // Opponent's counters must NOT be counted — this is the filter's discriminator.
    scenario
        .add_creature(P1, "Rival Bear", 2, 2)
        .with_plus_counters(2);
    let runner = scenario.build();

    let count = monstrosity_count();
    let resolved = resolve_quantity(runner.state(), &count, P0, source);

    assert_eq!(
        resolved, 7,
        "CR 122.1: X must count the counters among creatures YOU control (4 + 3 = 7). 0 means the \
         Monstrosity count is still an unbound Variable(\"X\") and the creature becomes monstrous \
         with no counters at all; 9 means the 'you control' filter was dropped and the opponent's \
         2 were counted. Got {resolved} from {count:?}"
    );
}

/// Pull Monstrosity's bound count out of Maester Seymour's real activated ability.
fn monstrosity_count() -> QuantityExpr {
    let parsed = parse_oracle_text(
        "{3}{G}{G}: Monstrosity X, where X is the number of counters among creatures you control.",
        "Maester Seymour",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let def = parsed
        .abilities
        .first()
        .expect("Maester Seymour's activated ability must parse");
    match &*def.effect {
        Effect::Monstrosity { count, .. } => count.clone(),
        other => panic!(
            "expected Monstrosity, got {other:?} — a where_x_binding gap here means the count \
             never bound and this witness would be testing nothing"
        ),
    }
}

// ---------------------------------------------------------------------------
// THE COST-X SIBLING (#96) — the X/X token minted by an X-cost spell's own ETB.
//
// This is NOT the where-X walk. These cards carry no "where X is …" tail at all; their X
// is the X PAID TO CAST THE SPELL, and the rewrite that owns it is
// `rewrite_cost_x_in_effect` (`Variable("X")` -> `CostXPaid`).
//
// That walk rewrote `Effect::Token`'s `count` but never its `power`/`toughness`, so an X/X
// token kept a bare `Variable("X")` in its P/T — which resolves to 0. Arboreal Alliance's
// Treefolk entered as an **0/0** and died instantly to state-based actions (CR 704.5f)
// while the card rendered as fully supported.
//
// SCOPE, stated so this is not over-read: this closes the Token P/T slot, NOT #96's class.
// `rewrite_cost_x_in_effect` still has a `_ => {}` wildcard, and its CALLER is gated by
// `trigger_should_rewrite_cost_x` to ChangesZone->Battlefield self-ETB triggers only — so a
// cost-X trigger that is not a self-ETB (Shark Typhoon's CYCLING trigger) never reaches
// this walk at all and is still an 0/0 today. #96 stays open. See the report for why that
// one is a two-part engine change rather than a parser arm.
// ---------------------------------------------------------------------------

/// CR 107.3m + CR 704.5f — Arboreal Alliance. "When this enchantment enters, create an X/X
/// green Treefolk creature token."
///
/// The witness casts for X=4 and asserts the token's power resolves to 4. `CostXPaid` reads
/// `cost_x_paid` off the source object (stamped by `finalize_cast`, CR 107.3m); an
/// unrewritten `Variable("X")` reads nothing and falls through to **0**. So 0 is precisely
/// the pre-fix bug, and the assertion cannot pass while the slot is a bare placeholder.
#[test]
fn cost_x_token_pt_is_x_by_x_not_zero_by_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Arboreal Alliance", 1, 1).id();
    let mut runner = scenario.build();

    // CR 107.3m: the {X} paid to cast the spell, stashed on the permanent.
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("source object")
        .cost_x_paid = Some(4);

    let power = cost_x_token_power();
    let resolved = resolve_quantity(runner.state(), &power, P0, source);

    assert_eq!(
        resolved, 4,
        "CR 107.3m: the Treefolk must be a 4/4 — X is the X paid to cast the spell. 0 means the \
         token's P/T is still a bare Variable(\"X\") that rewrite_cost_x_in_effect never rewrote \
         (it rewrote only the token's `count`), so the Treefolk enters 0/0 and dies instantly to \
         SBAs (CR 704.5f) while the card reads as fully supported. Got {resolved} from {power:?}"
    );
}

/// Pull the ETB token's power out of Arboreal Alliance's real Oracle text.
fn cost_x_token_power() -> QuantityExpr {
    let parsed = parse_oracle_text(
        "When this enchantment enters, create an X/X green Treefolk creature token.\nWhenever you \
         attack with one or more Elves, populate.",
        "Arboreal Alliance",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let power = parsed
        .triggers
        .iter()
        .find_map(|trigger| match &*trigger.execute.as_ref()?.effect {
            Effect::Token { power, .. } => Some(power.clone()),
            _ => None,
        })
        .expect("Arboreal Alliance must lower a token-creating ETB trigger");

    match power {
        PtValue::Quantity(quantity) => quantity,
        other => panic!(
            "the Treefolk's power must lower to a dynamic quantity, not {other:?} — a bare \
             PtValue::Variable(\"X\") here means the cost-X P/T rewrite never ran"
        ),
    }
}

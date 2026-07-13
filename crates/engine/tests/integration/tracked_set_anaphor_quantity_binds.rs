//! CR 608.2c — the "<noun> <participle> this way" anaphor must aggregate over the
//! chain tracked set the preceding effect published, not resolve to 0.
//!
//! Follow-up to #57 / PR #5721 (task #78). `QuantityRef::TrackedSetAggregate`
//! and its live resolver both already existed, but the parser's anaphor list
//! recognized only the EXILE forms ("those exiled cards", "the card exiled this
//! way"). Every other participle — discarded, sacrificed, milled — fell through
//! to the `where_x_binding` / effect-level honest red, so the surrounding clause
//! was dropped entirely:
//!
//!   Ill-Timed Explosion    "the greatest mana value among cards discarded this way"
//!   Sword of the Ages      "the total power of the creatures sacrificed this way"
//!   Reign of the Pit       (same expression)
//!   Fateful Tempest        "the total mana value of cards milled this way"
//!   Shadowgrange Archfiend "the greatest power among creatures sacrificed this way"
//!
//! These are RUNTIME witnesses, not AST-shape assertions: each parses the card's
//! real Oracle clause through the production parser and hands the resulting
//! `QuantityExpr` to the live resolver (`game::quantity::resolve_quantity`)
//! against a state whose chain tracked set is actually populated. Before the
//! bind the clause never reached a `TrackedSetAggregate` at all, so the resolver
//! saw a raw-text `QuantityRef::Variable` and returned 0 — which is exactly what
//! each assertion discriminates against.
//!
//! The participle list is deliberately restricted to the causes the engine
//! actually stamps. "goaded" is NOT bound: `game/effects/goad.rs` publishes no
//! tracked set, so Havoc Eater's "the total power of creatures goaded this way"
//! would aggregate over an empty set and silently read 0. It stays an honest red.
//!
//! Oracle text below is read from the card export, not from memory.

use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, QuantityExpr};
use engine::types::identifiers::TrackedSetId;
use engine::types::mana::ManaCost;
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
        other => panic!("unexpected effect for {oracle:?}: {other:?}"),
    }
}

/// CR 608.2c: "the total power of the creatures sacrificed this way"
/// (Sword of the Ages, Reign of the Pit).
///
/// `Sum` over the chain-published set. Two creatures with power 3 and 4 were
/// sacrificed, so X is 7 — not 0.
#[test]
fn this_way_sacrificed_aggregates_total_power_not_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Sword of the Ages", 1, 1).id();
    let a = scenario.add_creature(P0, "Grizzly Bears", 3, 3).id();
    let b = scenario.add_creature(P0, "Hill Giant", 4, 4).id();
    let mut runner = scenario.build();

    // The preceding chain effect ("Sacrifice this artifact and any number of
    // creatures you control") publishes the sacrificed members as the chain set.
    runner
        .state_mut()
        .tracked_object_sets
        .insert(TrackedSetId(1), vec![a, b]);

    let expr = only_quantity(
        "~ deals X damage to any target, where X is the total power of the creatures sacrificed this way.",
    );
    let resolved = resolve_quantity(runner.state(), &expr, P0, source);

    assert_eq!(
        resolved, 7,
        "X must sum the power of the creatures actually sacrificed this way (3 + 4). \
         Before the bind the clause never reached a TrackedSetAggregate and the \
         raw-text QuantityRef::Variable resolved to 0 — a silent no-op that still \
         read as supported. Got {resolved} from {expr:?}"
    );
}

/// CR 608.2c + CR 202.3: "the greatest mana value among cards discarded this way"
/// (Ill-Timed Explosion).
///
/// `Max` over the same chain set, on the ManaValue property. The discarded cards
/// have mana value 2 and 5, so X is 5.
#[test]
fn this_way_discarded_aggregates_greatest_mana_value_not_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Ill-Timed Explosion", 1, 1).id();
    // The mana values must be established explicitly: `add_creature` leaves the
    // mana cost empty, and `ObjectProperty::ManaValue` reads
    // `GameObject::effective_mana_value()` off that cost. Without this the
    // aggregate would read 0 for every member and the assertion below would
    // pass for the WRONG reason (0 == 0) — a vacuous witness.
    let cheap = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let pricey = scenario
        .add_creature(P0, "Serra Angel", 4, 4)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .tracked_object_sets
        .insert(TrackedSetId(1), vec![cheap, pricey]);

    let expr = only_quantity(
        "~ deals X damage to any target, where X is the greatest mana value among cards discarded this way.",
    );
    let resolved = resolve_quantity(runner.state(), &expr, P0, source);

    assert_eq!(
        resolved, 5,
        "X must be the GREATEST mana value among the cards actually discarded this \
         way (max of 2 and 5), not 0. Got {resolved} from {expr:?}"
    );
}

/// CR 608.2c: the SINGULAR "this way" referent — "the power of the creature
/// exiled this way" (Astarion's Thirst) / "the mana value of the permanent
/// exiled this way" (Ruinous Intrusion).
///
/// No aggregate adjective is printed, so this cannot ride the extremum/total
/// prefix. It reads the same chain set; `Sum` over a one-member set is that
/// member's value.
#[test]
fn this_way_singular_referent_reads_the_one_member_not_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Astarion's Thirst", 1, 1).id();
    let exiled = scenario.add_creature(P0, "Hill Giant", 6, 4).id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .tracked_object_sets
        .insert(TrackedSetId(1), vec![exiled]);

    let expr =
        only_quantity("You gain X life, where X is the power of the creature exiled this way.");
    let resolved = resolve_quantity(runner.state(), &expr, P0, source);

    assert_eq!(
        resolved, 6,
        "X must read the power of the single creature exiled this way (6), not 0. \
         Got {resolved} from {expr:?}"
    );
}

/// Negative control: "creatures goaded this way" (Havoc Eater) must NOT bind.
///
/// `game/effects/goad.rs` publishes no tracked set, so a `TrackedSetAggregate`
/// here would aggregate over whatever unrelated set happened to be published
/// last — or over nothing — and silently resolve to 0 while reading as
/// supported. Binding it would manufacture exactly the lying-green class that
/// #57 exists to eliminate. It must stay an honest red until goad publishes its
/// set.
///
/// This asserts the PARSE, not the resolve: the clause must not lower to a
/// quantity at all.
#[test]
fn goaded_this_way_stays_an_honest_red_until_goad_publishes_a_set() {
    let parsed = parse_oracle_text(
        "Put X +1/+1 counters on ~, where X is the total power of creatures goaded this way.",
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
        "\"creatures goaded this way\" must remain an honest Effect::unimplemented gap: \
         goad publishes no tracked set, so binding it to TrackedSetAggregate would \
         resolve to 0 while rendering as supported. Parsed: {:?}",
        parsed.abilities
    );
}

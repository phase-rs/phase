//! #3223 — "This ability costs {N} less to activate if [condition]" on
//! non-equip activated abilities.
//!
//! Root cause (fixed): `strip_suffix_conditional` peeled the trailing
//! "if [condition]" off the self cost-reduction sentence UPSTREAM, leaving the
//! chained `Unimplemented` node's description as the bare
//! "This ability costs {N} less to activate" — which `try_parse_cost_reduction`
//! (whose conditional arm requires the full "… if [condition]" sentence) could
//! not match. The fix declines that peel for self cost-reduction sentences (via
//! `is_self_cost_reduction_prefix`) so the whole sentence reaches the existing
//! conditional arm, which re-homes the condition into `CostReduction.condition`.
//!
//! CR 601.2f: cost reductions are folded into the total cost at
//! cost-determination time. CR 602.2b: an activated ability's activation cost is
//! the analog of a spell's mana cost.
//!
//! These tests drive the real `parse_oracle_text` entry point with the verbatim
//! Oracle text from `data/mtgjson/AtomicCards.json`. They are DISCRIMINATING:
//! each asserts the dropped clause is now captured (and that no `Unimplemented`
//! node carrying the cost-reduction text survives), so they fail if the fix is
//! reverted. The negative tests enforce coverage-honesty: cards whose condition
//! the parser does not model MUST stay a loud gap (no silent mis-parse).

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityDefinition, CostReduction, Effect, QuantityExpr};

/// Walk an ability and its chained `sub_ability` nodes, returning the first
/// `CostReduction` found at any level (cost reduction is extracted onto the
/// ability that owns the reducible cost, which may be a chained sub-ability).
fn find_cost_reduction(def: &AbilityDefinition) -> Option<&CostReduction> {
    let mut cur = Some(def);
    while let Some(d) = cur {
        if let Some(cr) = d.cost_reduction.as_ref() {
            return Some(cr);
        }
        cur = d.sub_ability.as_deref();
    }
    None
}

/// True if any effect node reachable through this ability's `sub_ability` chain
/// is an `Unimplemented` gap whose description mentions "less to activate".
fn has_surviving_cost_reduction_gap(def: &AbilityDefinition) -> bool {
    let mut cur = Some(def);
    while let Some(d) = cur {
        if let Some(desc) = effect_unimplemented_text(d.effect.as_ref()) {
            if desc.to_lowercase().contains("less to activate") {
                return true;
            }
        }
        cur = d.sub_ability.as_deref();
    }
    false
}

fn effect_unimplemented_text(effect: &Effect) -> Option<&str> {
    effect.unimplemented_description()
}

/// Locate the first activated ability that carries a cost reduction at any
/// level of its chain.
fn ability_with_cost_reduction(oracle: &str, name: &str) -> AbilityDefinition {
    let parsed = parse_oracle_text(oracle, name, &[], &[], &[]);
    parsed
        .abilities
        .into_iter()
        .find(|a| find_cost_reduction(a).is_some())
        .unwrap_or_else(|| panic!("{name}: no activated ability captured a cost reduction"))
}

fn assert_flat_conditional(reduction: &CostReduction, expected_amount: u32, card: &str) {
    assert_eq!(
        reduction.amount_per, expected_amount,
        "{card}: amount_per should be {expected_amount}"
    );
    assert_eq!(
        reduction.count,
        QuantityExpr::Fixed { value: 1 },
        "{card}: flat conditional form has count = Fixed(1)"
    );
    assert!(
        reduction.condition.is_some(),
        "{card}: the 'if [condition]' gate must be captured, got {:?}",
        reduction.condition
    );
}

#[test]
fn esquire_of_the_king_cost_reduction_carries_legendary_condition() {
    let card = "Esquire of the King";
    let oracle = "{4}{W}, {T}: Creatures you control get +1/+1 until end of turn. \
This ability costs {2} less to activate if you control a legendary creature.";
    let ability = ability_with_cost_reduction(oracle, card);
    let reduction = find_cost_reduction(&ability).unwrap();
    assert_flat_conditional(reduction, 2, card);
    assert!(
        !has_surviving_cost_reduction_gap(&ability),
        "{card}: no Unimplemented 'less to activate' node should survive"
    );
}

#[test]
fn starport_security_cost_reduction_carries_counter_condition() {
    let card = "Starport Security";
    let oracle = "{3}{W}, {T}: Tap another target creature. \
This ability costs {2} less to activate if you control a creature with a +1/+1 counter on it.";
    let ability = ability_with_cost_reduction(oracle, card);
    let reduction = find_cost_reduction(&ability).unwrap();
    assert_flat_conditional(reduction, 2, card);
    assert!(
        !has_surviving_cost_reduction_gap(&ability),
        "{card}: no Unimplemented 'less to activate' node should survive"
    );
}

#[test]
fn raft_security_officer_cost_reduction_carries_target_power_condition() {
    let card = "Raft Security Officer";
    let oracle = "{2}, {T}: Tap target creature. \
This ability costs {1} less to activate if it targets a creature with power 3 or less.";
    let ability = ability_with_cost_reduction(oracle, card);
    let reduction = find_cost_reduction(&ability).unwrap();
    assert_flat_conditional(reduction, 1, card);
    assert!(
        !has_surviving_cost_reduction_gap(&ability),
        "{card}: no Unimplemented 'less to activate' node should survive"
    );
}

#[test]
fn razorlash_transmogrant_cost_reduction_carries_opponent_lands_condition() {
    let card = "Razorlash Transmogrant";
    let oracle = "This creature can't block.\n\
{4}{B}{B}: Return this card from your graveyard to the battlefield with a +1/+1 counter on it. \
This ability costs {4} less to activate if an opponent controls four or more nonbasic lands.";
    let ability = ability_with_cost_reduction(oracle, card);
    let reduction = find_cost_reduction(&ability).unwrap();
    assert_flat_conditional(reduction, 4, card);
    assert!(
        !has_surviving_cost_reduction_gap(&ability),
        "{card}: no Unimplemented 'less to activate' node should survive"
    );
}

// --- Coverage-honesty negatives: unmodeled conditions MUST stay a loud gap. ---

/// Asserts no ability in the parse captured a cost reduction, and that an
/// `Unimplemented` "less to activate" gap survives somewhere in the chains.
fn assert_cost_reduction_stays_gapped(oracle: &str, card: &str) {
    let parsed = parse_oracle_text(oracle, card, &[], &[], &[]);
    assert!(
        parsed
            .abilities
            .iter()
            .all(|a| find_cost_reduction(a).is_none()),
        "{card}: unmodeled condition must NOT produce a cost reduction (coverage honesty)"
    );
    assert!(
        parsed
            .abilities
            .iter()
            .any(has_surviving_cost_reduction_gap),
        "{card}: the cost-reduction clause must survive as a loud Unimplemented gap"
    );
}

#[test]
fn a_sewer_crocodile_cost_reduction_stays_gapped() {
    // Unmodeled condition: "if there are five or more mana values among cards in
    // your graveyard."
    let card = "A-Sewer Crocodile";
    let oracle = "{3}{U}: Sewer Crocodile can't be blocked this turn. \
This ability costs {3} less to activate if there are five or more mana values among cards in your graveyard.";
    assert_cost_reduction_stays_gapped(oracle, card);
}

#[test]
fn wayta_trainer_prodigy_cost_reduction_stays_gapped() {
    // Unmodeled condition: "if it targets two creatures you control."
    let card = "Wayta, Trainer Prodigy";
    let oracle = "Haste\n\
{2}{G}, {T}: Target creature you control fights another target creature. \
This ability costs {2} less to activate if it targets two creatures you control.\n\
If a creature you control being dealt damage causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.";
    assert_cost_reduction_stays_gapped(oracle, card);
}

#[test]
fn thaumaton_torpedo_cost_reduction_stays_gapped() {
    // Unmodeled condition: "if you attacked with a Spacecraft this turn."
    let card = "Thaumaton Torpedo";
    let oracle = "{6}, {T}, Sacrifice this artifact: Destroy target nonland permanent. \
This ability costs {3} less to activate if you attacked with a Spacecraft this turn.";
    assert_cost_reduction_stays_gapped(oracle, card);
}

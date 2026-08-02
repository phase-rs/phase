//! Two-layer IR + lowered parity snapshot tests (Phase 51, D-03/D-04).
//!
//! Each test parses real card Oracle text through parse_oracle_ir (producing
//! OracleDocIr) and lower_oracle_ir (producing ParsedAbilities), snapshotting
//! both layers so structural drift and assembly bugs are independently caught.

use crate::parser::oracle::{lower_oracle_ir, parse_oracle_ir, ParsedAbilities};
use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;
use crate::parser::oracle_ir::doc::{OracleDocIr, OracleNodeIr};
use crate::parser::oracle_ir::trigger::TriggerNodeIr;
use crate::types::ability::MultiTargetSpec;
use crate::types::ability::{
    AbilityCost, ActivationRestriction, Effect, TargetChoiceTiming, TriggerCondition,
};
use crate::types::game_state::DistributionUnit;

fn ability_has_unimplemented(def: &crate::types::ability::AbilityDefinition) -> bool {
    matches!(def.effect.as_ref(), Effect::Unimplemented { .. })
        || def
            .sub_ability
            .as_deref()
            .is_some_and(ability_has_unimplemented)
        || def
            .else_ability
            .as_deref()
            .is_some_and(ability_has_unimplemented)
}

/// Parse Oracle text through both IR and lowering layers.
fn parse_two_layer(
    oracle_text: &str,
    card_name: &str,
    types: &[&str],
    subtypes: &[&str],
) -> (OracleDocIr, ParsedAbilities) {
    parse_two_layer_with_keywords(oracle_text, card_name, &[], types, subtypes)
}

fn parse_two_layer_with_keywords(
    oracle_text: &str,
    card_name: &str,
    keywords: &[&str],
    types: &[&str],
    subtypes: &[&str],
) -> (OracleDocIr, ParsedAbilities) {
    let keywords: Vec<String> = keywords.iter().map(|s| s.to_string()).collect();
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    let mut ir = parse_oracle_ir(oracle_text, card_name, &keywords, &types, &subtypes);
    let lowered = lower_oracle_ir(&mut ir);
    (ir, lowered)
}

/// CR 707.9a + CR 602.1a: generic activated abilities are emitted as native
/// spell IR, so the source-ordered lowerer can stamp the second printed ability
/// before its self-retention copy effect is finalized.
#[test]
fn thespians_stage_generic_activated_router_is_ir_native() {
    let (ir, lowered) = parse_two_layer(
        "{T}: Add {C}.\n{2}, {T}: This land becomes a copy of target land, except it has this ability.",
        "Thespian's Stage",
        &["Land"],
        &[],
    );

    assert_eq!(ir.items.len(), 2);
    assert!(matches!(&ir.items[0].node, OracleNodeIr::Spell(_)));
    assert!(matches!(&ir.items[1].node, OracleNodeIr::Spell(_)));
    assert_eq!(lowered.abilities.len(), 2);
    assert!(
        !ability_has_unimplemented(&lowered.abilities[1]),
        "the copy ability must lower without a residual fallback: {:?}",
        lowered.abilities[1]
    );
    assert!(matches!(
        lowered.abilities[1].effect.as_ref(),
        Effect::BecomeCopy { additional_modifications, .. }
            if additional_modifications.iter().any(|modification| matches!(
                modification,
                crate::types::ability::ContinuousModification::RetainPrintedAbilityFromSource {
                    source_ability_index: 1
                }
            ))
    ));

    insta::assert_json_snapshot!("thespians_stage_generic_activated_ir", &ir);
    insta::assert_json_snapshot!("thespians_stage_generic_activated_lowered", &lowered);
}

/// CR 207.2c + CR 602.1: an ability-word label on an activated ability does
/// not impose a condition; the generic activation envelope is retained in IR.
#[test]
fn barbarian_ring_ability_word_activated_router_is_ir_native() {
    let (ir, lowered) = parse_two_layer(
        "{T}: Add {R}. This land deals 1 damage to you.\nThreshold — {R}, {T}, Sacrifice this land: It deals 2 damage to any target. Activate only if there are seven or more cards in your graveyard.",
        "Barbarian Ring",
        &["Land"],
        &[],
    );

    assert_eq!(ir.items.len(), 2);
    let OracleNodeIr::Spell(activated) = &ir.items[1].node else {
        panic!(
            "expected native activated spell IR, got {:?}",
            ir.items[1].node
        );
    };
    assert!(!activated.shell.activation_restrictions.is_empty());
    assert_eq!(lowered.abilities.len(), 2);
    let ability = &lowered.abilities[1];
    assert!(
        !ability_has_unimplemented(ability),
        "ability-word activated route must not fall back: {ability:?}"
    );
    assert!(
        ability.condition.is_none(),
        "Threshold has no rules meaning here"
    );
    assert!(matches!(
        ability.cost.as_ref(),
        Some(crate::types::ability::AbilityCost::Composite { .. })
    ));
    assert!(matches!(ability.effect.as_ref(), Effect::DealDamage { .. }));
    assert!(
        !ability.activation_restrictions.is_empty(),
        "explicit Activate only restriction must survive"
    );

    insta::assert_json_snapshot!("barbarian_ring_activated_ir", &ir);
    insta::assert_json_snapshot!("barbarian_ring_activated_lowered", &lowered);
}

/// CR 706.3b: an activated terminal die roll owns only its contiguous result
/// rows and preserves them as native branch IR until final lowering.
#[test]
fn component_pouch_activated_die_table_is_ir_native() {
    let (ir, lowered) = parse_two_layer(
        "{T}, Remove a component counter from this artifact: Add two mana of different colors.\n{T}: Roll a d20.\n1—9 | Put a component counter on this artifact.\n10—20 | Put two component counters on this artifact.",
        "Component Pouch",
        &["Artifact"],
        &[],
    );

    assert_eq!(ir.items.len(), 2, "result rows must not become items");
    let OracleNodeIr::Spell(roll) = &ir.items[1].node else {
        panic!(
            "expected native roll ability IR, got {:?}",
            ir.items[1].node
        );
    };
    assert_eq!(
        roll.die_results
            .iter()
            .map(|branch| (branch.min, branch.max))
            .collect::<Vec<_>>(),
        vec![(1, 9), (10, 20)]
    );
    assert!(matches!(
        lowered.abilities[1].effect.as_ref(),
        Effect::RollDie { results, .. } if results.len() == 2
    ));

    insta::assert_json_snapshot!("component_pouch_activated_ir", &ir);
    insta::assert_json_snapshot!("component_pouch_activated_lowered", &lowered);
}

/// CR 706.3b: the typed terminal-roll guard must leave the next line to the
/// ordinary router when a die roll has a following non-roll instruction.
#[test]
fn nonterminal_activated_die_roll_does_not_consume_following_ability() {
    let (ir, lowered) = parse_two_layer(
        "{T}: Roll a d20, then draw a card.\n{T}: Add {C}.",
        "Nonterminal Activated Die Fixture",
        &["Artifact"],
        &[],
    );

    assert_eq!(
        ir.items.len(),
        2,
        "the following activation must remain routable"
    );
    assert!(matches!(&ir.items[0].node, OracleNodeIr::Spell(_)));
    assert!(matches!(&ir.items[1].node, OracleNodeIr::Spell(_)));
    assert_eq!(lowered.abilities.len(), 2);
    let first = &lowered.abilities[0];
    assert!(matches!(first.effect.as_ref(), Effect::RollDie { .. }));
    assert!(matches!(
        first.sub_ability.as_deref().map(|sub| sub.effect.as_ref()),
        Some(Effect::Draw { .. })
    ));
    assert!(
        lowered
            .abilities
            .iter()
            .all(|ability| !ability_has_unimplemented(ability)),
        "both ordinary activations must parse after the nonterminal roll: {:?}",
        lowered.abilities
    );
    assert!(matches!(
        lowered.abilities[1].effect.as_ref(),
        Effect::Mana { .. }
    ));
}

/// CR 700.3 + CR 701.38: Priority 9 keeps its pile and vote roots native until
/// document lowering; their nested per-choice and chosen-pile payloads remain
/// deliberately pre-lowered effect internals.
#[test]
fn priority_nine_spell_router_keeps_vote_and_pile_roots_native() {
    let (vote_ir, vote_lowered) = parse_two_layer(
        "Starting with you, each player votes for evidence or bribery. For each evidence vote, investigate. For each bribery vote, create a Treasure token.",
        "Vote Spell Fixture",
        &["Sorcery"],
        &[],
    );
    assert!(matches!(vote_ir.items[0].node, OracleNodeIr::Spell(_)));
    assert!(matches!(
        vote_lowered.abilities[0].effect.as_ref(),
        Effect::Vote { .. }
    ));

    let (pile_ir, pile_lowered) = parse_two_layer(
        "Reveal the top five cards of your library. An opponent separates those cards into two piles. Put one pile into your hand and the other into your graveyard.",
        "Fact or Fiction",
        &["Instant"],
        &[],
    );
    assert!(matches!(pile_ir.items[0].node, OracleNodeIr::Spell(_)));
    assert!(matches!(
        pile_lowered.abilities[0].effect.as_ref(),
        Effect::SeparateIntoPiles { .. }
    ));
}

/// CR 601.2b: Priority 9 keeps all aggregate source text and the X floor on
/// the native node until document lowering.
#[test]
fn priority_nine_multiline_spell_keeps_description_and_x_floor_in_ir() {
    let oracle_text = "Draw a card.\nThen draw a card.\nX can't be 0.";
    let (ir, lowered) = parse_two_layer(oracle_text, "Multiline Spell Fixture", &["Sorcery"], &[]);
    let OracleNodeIr::Spell(ability) = &ir.items[0].node else {
        panic!("multiline spell must remain native IR");
    };
    assert!(ability.root_transforms.iter().any(|transform| matches!(
        transform,
        crate::parser::oracle_ir::effect_chain::AbilityRootTransform::SetMinXValue(1)
    )));
    assert!(ability.root_transforms.iter().any(|transform| matches!(
        transform,
        crate::parser::oracle_ir::effect_chain::AbilityRootTransform::SetDescription(description)
            if description == "Draw a card.\nThen draw a card."
    )));
    assert_eq!(lowered.abilities.len(), 1);
    assert_eq!(lowered.abilities[0].min_x_value, 1);
    assert_eq!(
        lowered.abilities[0].description.as_deref(),
        Some("Draw a card.\nThen draw a card.")
    );
}

/// CR 706.3b: ordinary trigger dispatch retains a die-result table in native
/// trigger IR and attaches it to the terminal roll before finalization.
#[test]
fn direct_trigger_die_table_is_ir_native_and_lowers_as_one_ability() {
    let (ir, lowered) = parse_two_layer(
        "Whenever this creature attacks, roll a d20.\n1—9 | Create a Treasure token. (It's an artifact with \"{T}, Sacrifice this token: Add one mana of any color.\")\n10—19 | Create two Treasure tokens.\n20 | Create three Treasure tokens.",
        "Hoarding Ogre",
        &["Creature"],
        &["Giant"],
    );

    assert_eq!(
        ir.items.len(),
        1,
        "result rows must not become document items"
    );
    let OracleNodeIr::Trigger(TriggerNodeIr::Parsed(trigger)) = &ir.items[0].node else {
        panic!(
            "expected a native parsed trigger, got {:?}",
            ir.items[0].node
        );
    };
    assert_eq!(trigger.die_results.len(), 3);
    assert_eq!(
        trigger
            .die_results
            .iter()
            .map(|branch| (branch.min, branch.max))
            .collect::<Vec<_>>(),
        vec![(1, 9), (10, 19), (20, 20)]
    );

    let execute = lowered.triggers[0]
        .execute
        .as_deref()
        .expect("Hoarding Ogre trigger must have an execute ability");
    let Effect::RollDie { results, .. } = execute.effect.as_ref() else {
        panic!(
            "expected terminal roll-die effect, got {:?}",
            execute.effect
        );
    };
    assert_eq!(results.len(), 3);
    assert!(
        results
            .iter()
            .all(|branch| !matches!(branch.effect.effect.as_ref(), Effect::Unimplemented { .. })),
        "table branches must be lowered through the ordinary ability authority: {results:?}"
    );
}

/// CR 603.2 + CR 706.3b: Each trigger produced by a compound trigger line owns
/// the following die-result table when its body ends in that line's die roll.
#[test]
fn compound_trigger_die_table_attaches_to_every_terminal_roll() {
    let (ir, lowered) = parse_two_layer(
        "Whenever this creature attacks and whenever this creature blocks, roll a d20.\n1—9 | Draw a card.\n10—20 | Create a Treasure token.",
        "Compound Die Fixture",
        &["Creature"],
        &[],
    );

    assert_eq!(
        ir.items.len(),
        2,
        "result rows must not become document items"
    );
    for item in &ir.items {
        let OracleNodeIr::Trigger(TriggerNodeIr::Parsed(trigger)) = &item.node else {
            panic!(
                "expected native parsed compound triggers, got {:?}",
                item.node
            );
        };
        assert_eq!(trigger.die_results.len(), 2);
    }

    assert_eq!(lowered.triggers.len(), 2);
    for trigger in &lowered.triggers {
        assert!(matches!(
            trigger.execute.as_deref().map(|execute| execute.effect.as_ref()),
            Some(Effect::RollDie { results, .. }) if results.len() == 2
        ));
    }

    let (shared_subject_ir, shared_subject_lowered) = parse_two_layer(
        "Whenever this creature attacks, blocks, or becomes the target of a spell, roll a d20.\n1—9 | Draw a card.\n10—20 | Create a Treasure token.",
        "Shared Subject Die Fixture",
        &["Creature"],
        &[],
    );
    assert_eq!(shared_subject_ir.items.len(), 3);
    assert_eq!(shared_subject_lowered.triggers.len(), 3);
    for (item, trigger) in shared_subject_ir
        .items
        .iter()
        .zip(&shared_subject_lowered.triggers)
    {
        let OracleNodeIr::Trigger(TriggerNodeIr::Parsed(parsed)) = &item.node else {
            panic!("expected a native parsed shared-subject trigger");
        };
        assert_eq!(parsed.die_results.len(), 2);
        assert!(matches!(
            trigger.execute.as_deref().map(|execute| execute.effect.as_ref()),
            Some(Effect::RollDie { results, .. }) if results.len() == 2
        ));
    }
}

/// CR 118.12 + CR 603.12 + CR 706.3b: A reflexive payment owns its nested
/// terminal die roll, so the parent trigger's result rows lower into that
/// `When you do` sub-ability.
#[test]
fn reflexive_payment_trigger_retains_die_table_on_nested_roll() {
    let (ir, lowered) = parse_two_layer(
        "Whenever this creature attacks, you may pay {1}. When you do, roll a d20.\n1—9 | Draw a card.\n10—20 | Create a Treasure token.",
        "Reflexive Die Fixture",
        &["Creature"],
        &[],
    );

    let OracleNodeIr::Trigger(TriggerNodeIr::Parsed(trigger)) = &ir.items[0].node else {
        panic!("expected a native parsed trigger");
    };
    assert!(trigger.has_terminal_roll_die());
    assert_eq!(trigger.die_results.len(), 2);

    let pay = lowered.triggers[0]
        .execute
        .as_deref()
        .expect("trigger must have an execute ability");
    let Effect::PayCost { .. } = pay.effect.as_ref() else {
        panic!("expected reflexive payment root, got {:?}", pay.effect);
    };
    let reflexive_roll = pay
        .sub_ability
        .as_deref()
        .expect("payment must retain its reflexive sub-ability");
    assert!(matches!(
        reflexive_roll.effect.as_ref(),
        Effect::RollDie { results, .. } if results.len() == 2
    ));
}

/// The ability-word trigger route is likewise native IR. Its existing fallback
/// condition is applied only when trigger parsing did not already find one.
#[test]
fn ability_word_trigger_table_is_ir_native_and_preserves_condition_fallback() {
    let (ir, lowered) = parse_two_layer(
        "Wild Magic Surge — Whenever this creature attacks, roll a d20.\n1—9 | Exile the top card of your library. You may play it this turn.\n10—19 | Exile the top two cards of your library. You may play them this turn.\n20 | Exile the top three cards of your library. You may play them this turn.",
        "Chaos Channeler",
        &["Creature"],
        &["Human", "Shaman"],
    );
    let OracleNodeIr::Trigger(TriggerNodeIr::Parsed(trigger)) = &ir.items[0].node else {
        panic!("expected a native parsed ability-word trigger");
    };
    assert_eq!(trigger.die_results.len(), 3);
    assert!(matches!(
        lowered.triggers[0].execute.as_deref().map(|execute| execute.effect.as_ref()),
        Some(Effect::RollDie { results, .. }) if results.len() == 3
    ));

    let (threshold_ir, threshold_lowered) = parse_two_layer(
        "Threshold — Whenever this creature attacks, draw a card.",
        "Threshold Fixture",
        &["Creature"],
        &[],
    );
    let OracleNodeIr::Trigger(TriggerNodeIr::Parsed(threshold)) = &threshold_ir.items[0].node
    else {
        panic!("expected a native parsed threshold trigger");
    };
    assert!(
        threshold.modifiers.intervening_if.is_none(),
        "fixture must exercise the ability-word fallback rather than an explicit if clause"
    );
    assert!(matches!(
        threshold_lowered.triggers[0].condition,
        Some(TriggerCondition::QuantityComparison { .. })
    ));

    let (shoreline_ir, shoreline_lowered) = parse_two_layer(
        "Delirium — Whenever this creature deals combat damage to a player, draw a card. Then discard a card unless there are seven or more cards in your graveyard.",
        "Shoreline Looter",
        &["Creature"],
        &["Rat", "Rogue"],
    );
    let OracleNodeIr::Trigger(TriggerNodeIr::Parsed(shoreline)) = &shoreline_ir.items[0].node
    else {
        panic!("expected a native parsed Shoreline Looter trigger");
    };
    assert!(shoreline.partial_def.condition.is_none());
    assert!(
        shoreline.modifiers.intervening_if.is_some(),
        "parsed intervening-if must suppress the ability-word fallback"
    );
    assert!(matches!(
        shoreline_lowered.triggers[0].condition,
        Some(TriggerCondition::Not { .. })
    ));
}

/// The table scanner consumes only actual rows. A following ordinary line stays
/// available to document dispatch, while unsupported table text remains honest.
#[test]
fn trigger_die_table_scanner_preserves_non_table_and_unsupported_rows() {
    let (non_table_ir, non_table_lowered) = parse_two_layer(
        "Whenever this creature attacks, roll a d20.\nDraw a card.",
        "Non-table Fixture",
        &["Creature"],
        &[],
    );
    assert_eq!(
        non_table_ir.items.len(),
        2,
        "ordinary next line must remain dispatched"
    );
    assert!(matches!(
        non_table_ir.items[0].node,
        OracleNodeIr::Trigger(_)
    ));
    assert!(matches!(non_table_ir.items[1].node, OracleNodeIr::Spell(_)));
    assert!(matches!(
        non_table_lowered.abilities[0].effect.as_ref(),
        Effect::Draw { .. }
    ));

    let (_, unsupported_lowered) = parse_two_layer(
        "Whenever this creature attacks, roll a d20.\n1—20 | Frobnicate target creature.",
        "Unsupported Table Fixture",
        &["Creature"],
        &[],
    );
    let execute = unsupported_lowered.triggers[0]
        .execute
        .as_deref()
        .expect("trigger must still lower");
    let Effect::RollDie { results, .. } = execute.effect.as_ref() else {
        panic!("expected roll die");
    };
    assert!(matches!(
        results[0].effect.effect.as_ref(),
        Effect::Unimplemented { .. }
    ));

    let (mastiff_ir, mastiff_lowered) = parse_two_layer(
        "Whenever this creature attacks, roll a d20 for each player being attacked and ignore all but the highest roll.\n1—9 | This creature deals damage equal to its power to you.\n10—19 | This creature deals damage equal to its power to defending player.\n20 | This creature deals damage equal to its power to each opponent.",
        "Iron Mastiff",
        &["Artifact", "Creature"],
        &["Dog"],
    );
    let OracleNodeIr::Trigger(TriggerNodeIr::Parsed(mastiff)) = &mastiff_ir.items[0].node else {
        panic!("expected a native parsed Iron Mastiff trigger");
    };
    assert!(
        !mastiff.has_terminal_roll_die(),
        "unsupported multi-player roll must not consume a result table"
    );
    assert_eq!(mastiff_ir.items.len(), 4);
    assert!(mastiff_ir.items[1..]
        .iter()
        .all(|item| matches!(item.node, OracleNodeIr::Unsupported { .. })));
    assert_eq!(mastiff_lowered.abilities.len(), 3);
}

/// ISSUES #17: the swallow audit's findings must live in the doc IR's diagnostics
/// channel, not be direct-appended to `ParsedAbilities::parse_warnings` behind the
/// doc's back.
///
/// The audit's *input* is the assembled result, so it necessarily runs after the
/// fold — but that is a reason to hand it the doc channel as its sink, not a reason
/// to give it a private one. `OracleDocIr.diagnostics` is the single warning
/// channel; `parse_warnings` is a copy of it.
///
/// Fixture is pool-verified, not synthetic: Boing!'s Oracle text is verbatim
/// MTGJSON, and it carries a live `DynamicQty` swallowed-clause warning — "scry
/// a number of cards equal to the result" lowers to a fixed `Scry` count, so
/// the die-result-dependent quantity is genuinely dropped from the parse. A
/// synthetic fixture could go vacuously green if the detector stopped firing;
/// this one cannot without that separately-tracked defect being fixed.
///
/// (Intermediate Chirography previously served as this fixture, but issue
/// #5638's fix taught `parse_class_oracle_text` to compose a level-gated
/// trigger's printed intervening-if with its `ClassLevelGE` condition instead
/// of overwriting it — the card's `Duration_ThisTurn` warning was that
/// overwrite silently dropping the "this turn" scoped condition, and is gone
/// now that the condition survives.)
#[test]
fn swallow_diagnostics_are_homed_in_the_doc_ir_channel() {
    let (ir, lowered) = parse_two_layer(
        "Return target creature to its owner's hand, then roll a six-sided die. \
         If the result is 3 or less, scry a number of cards equal to the result.",
        "Boing!",
        &["Instant"],
        &[],
    );

    // (a) The re-homing itself. Before this change the audit wrote to a private vec
    //     that was appended straight onto `parse_warnings`, so the doc channel never
    //     saw a swallowed clause and this assertion was unsatisfiable.
    assert!(
        ir.diagnostics
            .iter()
            .any(|d| matches!(d, OracleDiagnostic::SwallowedClause { .. })),
        "swallow audit must emit into OracleDocIr.diagnostics; got {:?}",
        ir.diagnostics
    );

    // (b) One channel, one order. `parse_warnings` is assigned FROM the doc channel,
    //     so any future direct-append to `parse_warnings` re-opens the bypass and
    //     fails here.
    assert_eq!(
        lowered.parse_warnings, ir.diagnostics,
        "parse_warnings must be a copy of OracleDocIr.diagnostics, not a separate sink"
    );
}

#[test]
fn forked_bolt_preserves_distribution_metadata_after_parse() {
    let (_, lowered) = parse_two_layer(
        "Forked Bolt deals 2 damage divided as you choose among one or two targets.",
        "Forked Bolt",
        &["Instant"],
        &[],
    );

    assert_eq!(
        lowered.abilities.len(),
        1,
        "Forked Bolt must lower one spell"
    );
    assert_eq!(
        lowered.abilities[0].distribute,
        Some(DistributionUnit::Damage),
        "Forked Bolt distribution metadata lost during document lowering"
    );
    assert_eq!(
        lowered.abilities[0].multi_target,
        Some(MultiTargetSpec::fixed(1, 2)),
        "Forked Bolt target-count metadata lost during document lowering"
    );
}

// ---------------------------------------------------------------------------
// Keywords
// ---------------------------------------------------------------------------

#[test]
fn serra_angel() {
    let (ir, lowered) = parse_two_layer(
        "Flying\nVigilance (Attacking doesn't cause this creature to tap.)",
        "Serra Angel",
        &["Creature"],
        &["Angel"],
    );
    insta::assert_json_snapshot!("serra_angel_ir", &ir);
    insta::assert_json_snapshot!("serra_angel_lowered", &lowered);
}

#[test]
fn baneslayer_angel() {
    let (ir, lowered) = parse_two_layer(
        "Flying, first strike, lifelink, protection from Demons and from Dragons",
        "Baneslayer Angel",
        &["Creature"],
        &["Angel"],
    );
    insta::assert_json_snapshot!("baneslayer_angel_ir", &ir);
    insta::assert_json_snapshot!("baneslayer_angel_lowered", &lowered);
}

#[test]
fn slippery_bogle() {
    let (ir, lowered) = parse_two_layer(
        "Hexproof (This creature can't be the target of spells or abilities your opponents control.)",
        "Slippery Bogle",
        &["Creature"],
        &["Beast"],
    );
    insta::assert_json_snapshot!("slippery_bogle_ir", &ir);
    insta::assert_json_snapshot!("slippery_bogle_lowered", &lowered);
}

#[test]
fn questing_beast() {
    let (ir, lowered) = parse_two_layer(
        "Vigilance, deathtouch, haste\nQuesting Beast can't be blocked by creatures with power 2 or less.\nCombat damage that would be dealt by creatures you control can't be prevented.\nWhenever Questing Beast deals combat damage to an opponent, it deals that much damage to target planeswalker that player controls.",
        "Questing Beast",
        &["Creature"],
        &["Beast"],
    );
    insta::assert_json_snapshot!("questing_beast_ir", &ir);
    insta::assert_json_snapshot!("questing_beast_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// CR 615.1a prevention spells — the instant/sorcery prevention recognizer
// ---------------------------------------------------------------------------
//
// CR 615.1a: "Effects that use the word 'prevent' are prevention effects."
// That sentence *is* this recognizer's admission test: it claims an
// instant/sorcery line containing both "prevent" and "damage" (excluding the
// CR 614.15 ability-word self-replacement printings) and lowers the whole line
// as a resolving spell chain rather than a standing replacement definition.
//
// **Why these two fixtures exist.** 153 cards in the pool reach that site and,
// before Plan 05b T9a, NOT ONE of them was snapshotted — the only spell path
// that lowered a whole ability body without `finalize_effect_chain`, the
// owner-library reveal anchor, and the `WithContext` whole-body recognizer set
// was also the one with no two-layer guard. T9a routed it through
// `lower_ability_ir` (via `ability_ir_at`) and measured a zero full-pool delta;
// these pin that result so T9b's payload swap — which lands on this exact
// recognizer — cannot move it silently.
//
// Both texts are verbatim MTGJSON, not paraphrases: a paraphrase can take a
// different parser branch and go green while the real card stays broken.

/// The canonical single-clause prevention spell — the whole card is the
/// prevention sentence, so the chain has exactly one clause and no `sub_ability`.
#[test]
fn fog_prevention_spell() {
    let (ir, lowered) = parse_two_layer(
        "Prevent all combat damage that would be dealt this turn.",
        "Fog",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("fog_ir", &ir);
    insta::assert_json_snapshot!("fog_lowered", &lowered);
}

/// The multi-clause case the recognizer was written for. The site's own comment
/// cites this shape verbatim — "preserve any preceding clauses ('You gain 1 life
/// for each ...')" — because the prevention marker sits in the SECOND sentence,
/// so a replacement classifier reaching the line first would drop the life gain.
/// This is the fixture that exercises chain assembly and `lower_ability_ir`'s
/// pinned chain → finalize → anchor → `sub_link` order, rather than a
/// degenerate one-clause body that would take the same path either way.
#[test]
fn blunt_the_assault_prevention_spell_preserves_preceding_clause() {
    let (ir, lowered) = parse_two_layer(
        "You gain 1 life for each creature on the battlefield. Prevent all combat damage that would be dealt this turn.",
        "Blunt the Assault",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("blunt_the_assault_ir", &ir);
    insta::assert_json_snapshot!("blunt_the_assault_lowered", &lowered);
}

/// CR 601.2b: a standalone "X can't be 0." annotation paragraph raises the
/// announced-X floor on the ability printed ABOVE it, and must do so without
/// converting that ability's node back to the pre-lowered shape.
///
/// DISCRIMINATING, and newly so. This line reaches
/// `DocEmitter::raise_last_spell_min_x`, which pops the last emitted spell item,
/// edits it, and re-emits it. Its predecessor — the general
/// `mutate_last_spell(f)` closure mutator — could only hand a closure an
/// `&mut AbilityDefinition`, so it had to LOWER the popped node first and could
/// only ever re-emit pre-lowered. Before T9b that was invisible, because the
/// only IR-native spell producer was unreachable from this line; after the
/// payload swap nine producers can precede it. Restore the closure mutator and
/// the `min_x_value` assertion still passes while the node assertion fails —
/// which is exactly the silent un-conversion this shape guards against.
///
/// The prevention line is the fixture because it is a *converted* producer
/// (U0-39), so `abilities[0]` is genuinely IR-native here; a fallback-parsed
/// line would emit the pre-lowered shape and make the node assertion vacuous.
///
/// Both layers are asserted on purpose. The IR half pins WHERE the floor is
/// stored (`AbilityShellIr::min_x_value`, pre-lowering); the lowered half pins
/// that `apply_ability_shell_envelope`'s `max` actually carries it onto the
/// root, so a floor parked in a shell field nothing reads cannot pass.
#[test]
fn a_standalone_x_floor_annotation_raises_an_ir_native_spells_floor() {
    let (ir, lowered) = parse_two_layer(
        "Prevent all combat damage that would be dealt this turn.\nX can't be 0.",
        "Probe",
        &["Instant"],
        &[],
    );

    // Reach-guard: exactly one ability, and it must be the IR-native node —
    // otherwise the floor assertion below says nothing about the `Spell` arm.
    assert_eq!(
        lowered.abilities.len(),
        1,
        "expected the prevention spell alone; the annotation paragraph is not an ability, got {:?}",
        lowered.abilities
    );
    assert!(
        matches!(ir.items[0].node, OracleNodeIr::Spell(_)),
        "the re-emitted node must stay IR-native, got {:?}",
        ir.items[0].node
    );

    assert_eq!(
        lowered.abilities[0].min_x_value, 1,
        "the \"X can't be 0.\" annotation must raise the lowered root's floor to 1"
    );
}

// ---------------------------------------------------------------------------
// Casting restrictions / permissions
// ---------------------------------------------------------------------------

#[test]
fn savage_summoning() {
    let (ir, lowered) = parse_two_layer(
        "This spell can't be countered.\nThe next creature spell you cast this turn can be cast as though it had flash. That spell can't be countered. That creature enters with an additional +1/+1 counter on it.",
        "Savage Summoning",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("savage_summoning_ir", &ir);
    insta::assert_json_snapshot!("savage_summoning_lowered", &lowered);
}

#[test]
fn leyline_of_anticipation() {
    let (ir, lowered) = parse_two_layer(
        "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou may cast spells as though they had flash.",
        "Leyline of Anticipation",
        &["Enchantment"],
        &[],
    );
    insta::assert_json_snapshot!("leyline_of_anticipation_ir", &ir);
    insta::assert_json_snapshot!("leyline_of_anticipation_lowered", &lowered);
}

#[test]
fn thalia_guardian_of_thraben() {
    let (ir, lowered) = parse_two_layer(
        "First strike\nNoncreature spells cost {1} more to cast.",
        "Thalia, Guardian of Thraben",
        &["Creature"],
        &["Human", "Soldier"],
    );
    insta::assert_json_snapshot!("thalia_guardian_of_thraben_ir", &ir);
    insta::assert_json_snapshot!("thalia_guardian_of_thraben_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Additional costs
// ---------------------------------------------------------------------------

#[test]
fn bone_splinters() {
    let (ir, lowered) = parse_two_layer(
        "As an additional cost to cast this spell, sacrifice a creature.\nDestroy target creature.",
        "Bone Splinters",
        &["Sorcery"],
        &[],
    );
    insta::assert_json_snapshot!("bone_splinters_ir", &ir);
    insta::assert_json_snapshot!("bone_splinters_lowered", &lowered);
}

#[test]
fn village_rites() {
    let (ir, lowered) = parse_two_layer(
        "As an additional cost to cast this spell, sacrifice a creature.\nDraw two cards.",
        "Village Rites",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("village_rites_ir", &ir);
    insta::assert_json_snapshot!("village_rites_lowered", &lowered);
}

#[test]
fn deadly_rollick() {
    let (ir, lowered) = parse_two_layer(
        "If you control a commander, you may cast this spell without paying its mana cost.\nExile target creature.",
        "Deadly Rollick",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("deadly_rollick_ir", &ir);
    insta::assert_json_snapshot!("deadly_rollick_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Activated abilities
// ---------------------------------------------------------------------------

#[test]
fn llanowar_elves() {
    let (ir, lowered) = parse_two_layer(
        "{T}: Add {G}.",
        "Llanowar Elves",
        &["Creature"],
        &["Elf", "Druid"],
    );
    insta::assert_json_snapshot!("llanowar_elves_ir", &ir);
    insta::assert_json_snapshot!("llanowar_elves_lowered", &lowered);
}

#[test]
fn mother_of_runes() {
    let (ir, lowered) = parse_two_layer(
        "{T}: Target creature you control gains protection from the color of your choice until end of turn.",
        "Mother of Runes",
        &["Creature"],
        &["Human", "Cleric"],
    );
    insta::assert_json_snapshot!("mother_of_runes_ir", &ir);
    insta::assert_json_snapshot!("mother_of_runes_lowered", &lowered);
}

#[test]
fn sylvan_safekeeper() {
    let (ir, lowered) = parse_two_layer(
        "Sacrifice a land: Target creature you control gains shroud until end of turn.",
        "Sylvan Safekeeper",
        &["Creature"],
        &["Human", "Wizard"],
    );
    insta::assert_json_snapshot!("sylvan_safekeeper_ir", &ir);
    insta::assert_json_snapshot!("sylvan_safekeeper_lowered", &lowered);
}

#[test]
fn jade_mage() {
    let (ir, lowered) = parse_two_layer(
        "{2}{G}: Create a 1/1 green Saproling creature token.",
        "Jade Mage",
        &["Creature"],
        &["Human", "Shaman"],
    );
    insta::assert_json_snapshot!("jade_mage_ir", &ir);
    insta::assert_json_snapshot!("jade_mage_lowered", &lowered);
}

#[test]
fn aetherling() {
    let (ir, lowered) = parse_two_layer(
        "{U}: Exile this creature. Return it to the battlefield under its owner's control at the beginning of the next end step.\n{U}: This creature can't be blocked this turn.\n{1}: This creature gets +1/-1 until end of turn.\n{1}: This creature gets -1/+1 until end of turn.",
        "Aetherling",
        &["Creature"],
        &["Shapeshifter"],
    );
    insta::assert_json_snapshot!("aetherling_ir", &ir);
    insta::assert_json_snapshot!("aetherling_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Planeswalker loyalty
// ---------------------------------------------------------------------------

#[test]
fn liliana_of_the_veil() {
    let (ir, lowered) = parse_two_layer(
        "[+1]: Each player discards a card.\n[\u{2212}2]: Target player sacrifices a creature.\n[\u{2212}6]: Separate all permanents target player controls into two piles. That player sacrifices all permanents in the pile of their choice.",
        "Liliana of the Veil",
        &["Planeswalker"],
        &["Liliana"],
    );
    insta::assert_json_snapshot!("liliana_of_the_veil_ir", &ir);
    insta::assert_json_snapshot!("liliana_of_the_veil_lowered", &lowered);
}

#[test]
fn jace_the_mind_sculptor() {
    let (ir, lowered) = parse_two_layer(
        "[+2]: Look at the top card of target player's library. You may put that card on the bottom of that player's library.\n[0]: Draw three cards, then put two cards from your hand on top of your library in any order.\n[\u{2212}1]: Return target creature to its owner's hand.\n[\u{2212}12]: Exile all cards from target player's library, then that player shuffles their hand into their library.",
        "Jace, the Mind Sculptor",
        &["Planeswalker"],
        &["Jace"],
    );
    insta::assert_json_snapshot!("jace_the_mind_sculptor_ir", &ir);
    insta::assert_json_snapshot!("jace_the_mind_sculptor_lowered", &lowered);
}

/// CR 606.3 + CR 606.5 + CR 107.3a: a `[−X]` loyalty header remains native
/// document IR, preserving its chosen-X loyalty-counter cost and sorcery-speed
/// activation envelope until the sole lowering seam.
#[test]
fn chandra_nalaar_minus_x_loyalty_is_ir_native() {
    let (ir, lowered) = parse_two_layer(
        "[−X]: Chandra Nalaar deals X damage to target creature.",
        "Chandra Nalaar",
        &["Planeswalker"],
        &["Chandra Nalaar", "Chandra"],
    );

    assert_eq!(ir.items.len(), 1);
    let OracleNodeIr::Spell(ability) = &ir.items[0].node else {
        panic!(
            "expected native loyalty spell IR, got {:?}",
            ir.items[0].node
        );
    };
    assert!(matches!(
        ability.shell.cost.as_ref(),
        Some(AbilityCost::RemoveCounter {
            count: crate::types::ability::REMOVE_COUNTER_COST_X,
            counter_type: crate::types::counter::CounterMatch::OfType(
                crate::types::counter::CounterType::Loyalty
            ),
            target: None,
            ..
        })
    ));
    assert_eq!(
        ability.shell.description.as_deref(),
        Some("[−X]: ~ deals X damage to target creature.")
    );
    assert_eq!(
        ability.shell.activation_restrictions,
        vec![ActivationRestriction::AsSorcery]
    );
    assert_eq!(lowered.abilities.len(), 1);
    assert!(matches!(
        lowered.abilities[0].cost.as_ref(),
        Some(AbilityCost::RemoveCounter {
            count: crate::types::ability::REMOVE_COUNTER_COST_X,
            counter_type: crate::types::counter::CounterMatch::OfType(
                crate::types::counter::CounterType::Loyalty
            ),
            target: None,
            ..
        })
    ));

    insta::assert_json_snapshot!("chandra_nalaar_minus_x_loyalty_ir", &ir);
    insta::assert_json_snapshot!("chandra_nalaar_minus_x_loyalty_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Spell temporal delayed triggers
// ---------------------------------------------------------------------------

/// CR 603.7a-c: spell-only temporal trigger lines stay as native document IR
/// until the sole lowering seam. The Pact payload remains a deliberately
/// lowered boxed ability inside the outer delayed-trigger clause.
#[test]
fn temporal_delayed_trigger_spell_router_is_ir_native() {
    // The three established grammar representatives remain the stable IR/lowered
    // snapshot fixtures. The direct `Whenever … this turn` arm uses the same
    // structural assertions without a fourth snapshot pair.
    let cases = [
        (
            None,
            "Whenever you cast a creature spell this turn, draw a card.",
            "Glimpse of Nature",
            &["Sorcery"][..],
        ),
        (
            Some((
                "pact_of_negation_temporal_ir",
                "pact_of_negation_temporal_lowered",
            )),
            "At the beginning of your next upkeep, pay {3}{U}{U}. If you don't, you lose the game.",
            "Pact of Negation",
            &["Instant"][..],
        ),
        (
            Some((
                "full_throttle_temporal_ir",
                "full_throttle_temporal_lowered",
            )),
            "At the beginning of each combat this turn, untap all creatures that attacked this turn.",
            "Full Throttle",
            &["Sorcery"][..],
        ),
        (
            Some((
                "galvanic_iteration_temporal_ir",
                "galvanic_iteration_temporal_lowered",
            )),
            "When you next cast an instant or sorcery spell this turn, copy that spell. You may choose new targets for the copy.",
            "Galvanic Iteration",
            &["Instant"][..],
        ),
    ];

    for (snapshots, oracle_text, card_name, types) in cases {
        let (ir, lowered) = parse_two_layer(oracle_text, card_name, types, &[]);
        assert_eq!(
            ir.items.len(),
            1,
            "{card_name}: one source line emits one item"
        );
        let OracleNodeIr::Spell(ability) = &ir.items[0].node else {
            panic!("{card_name}: expected native temporal spell IR");
        };
        assert!(matches!(
            &ability.body.clauses[0].parsed.effect,
            Effect::CreateDelayedTrigger { .. }
        ));
        assert_eq!(
            lowered.abilities.len(),
            1,
            "{card_name}: one lowered ability"
        );
        assert!(matches!(
            lowered.abilities[0].effect.as_ref(),
            Effect::CreateDelayedTrigger { .. }
        ));

        if card_name == "Pact of Negation" {
            let Effect::CreateDelayedTrigger { effect, .. } =
                &ability.body.clauses[0].parsed.effect
            else {
                unreachable!("checked above");
            };
            assert!(matches!(
                effect.kind,
                crate::types::ability::AbilityKind::Spell
            ));
        }

        if let Some((ir_snapshot, lowered_snapshot)) = snapshots {
            insta::with_settings!({ snapshot_suffix => ir_snapshot }, {
                insta::assert_json_snapshot!("temporal_delayed_trigger", &ir);
            });
            insta::with_settings!({ snapshot_suffix => lowered_snapshot }, {
                insta::assert_json_snapshot!("temporal_delayed_trigger", &lowered);
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Equipment / Vehicles
// ---------------------------------------------------------------------------

#[test]
fn short_sword() {
    let (ir, lowered) = parse_two_layer(
        "Equipped creature gets +1/+1.\nEquip {1} ({1}: Attach to target creature you control. Equip only as a sorcery.)",
        "Short Sword",
        &["Artifact"],
        &["Equipment"],
    );
    insta::assert_json_snapshot!("short_sword_ir", &ir);
    insta::assert_json_snapshot!("short_sword_lowered", &lowered);
}

#[test]
fn abraxas_named_equip() {
    let (ir, lowered) = parse_two_layer(
        "Abraxas — Equip {3}",
        "Named Equip",
        &["Artifact"],
        &["Equipment"],
    );
    insta::assert_json_snapshot!("abraxas_named_equip_ir", &ir);
    insta::assert_json_snapshot!("abraxas_named_equip_lowered", &lowered);
}

#[test]
fn smugglers_copter() {
    let (ir, lowered) = parse_two_layer(
        "Flying\nWhenever this Vehicle attacks or blocks, you may draw a card. If you do, discard a card.\nCrew 1 (Tap any number of creatures you control with total power 1 or more: This Vehicle becomes an artifact creature until end of turn.)",
        "Smuggler's Copter",
        &["Artifact"],
        &["Vehicle"],
    );
    insta::assert_json_snapshot!("smugglers_copter_ir", &ir);
    insta::assert_json_snapshot!("smugglers_copter_lowered", &lowered);
}

#[test]
fn thunderous_velocipede() {
    let (ir, lowered) = parse_two_layer_with_keywords(
        "Trample\nEach other Vehicle and creature you control enters with an additional +1/+1 counter on it if its mana value is 4 or less. Otherwise, it enters with three additional +1/+1 counters on it.\nCrew 3",
        "Thunderous Velocipede",
        &["trample", "crew"],
        &["Artifact"],
        &["Vehicle"],
    );
    insta::assert_json_snapshot!("thunderous_velocipede_ir", &ir);
    insta::assert_json_snapshot!("thunderous_velocipede_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Leveler
// ---------------------------------------------------------------------------

#[test]
fn student_of_warfare() {
    let (ir, lowered) = parse_two_layer(
        "Level up {W} ({W}: Put a level counter on this. Level up only as a sorcery.)\nLEVEL 2-6\n3/3\nFirst strike\nLEVEL 7+\n4/4\nDouble strike",
        "Student of Warfare",
        &["Creature"],
        &["Human", "Knight"],
    );
    insta::assert_json_snapshot!("student_of_warfare_ir", &ir);
    insta::assert_json_snapshot!("student_of_warfare_lowered", &lowered);
}

/// Leveler *body* static (Plan 05b, T2 witness).
///
/// `student_of_warfare` above reaches only the block-SUMMARY static
/// (`oracle_level.rs:194`), synthesized from P/T and keyword lines. Kabira
/// Vindicator prints a full sentence inside each LEVEL block, so it is the
/// witness for the body arm (`oracle_level.rs:154`, via `parse_static_line`)
/// — twice, once per block — while still carrying two block summaries.
///
/// The sibling multi arm (`:146`, `parse_static_line_multi`) has no pool
/// witness: no printed LEVEL body line lowers to more than one static.
#[test]
fn kabira_vindicator() {
    let (ir, lowered) = parse_two_layer(
        "Level up {2}{W} ({2}{W}: Put a level counter on this. Level up only as a sorcery.)\nLEVEL 2-4\n3/6\nOther creatures you control get +1/+1.\nLEVEL 5+\n4/8\nOther creatures you control get +2/+2.",
        "Kabira Vindicator",
        &["Creature"],
        &["Human", "Knight"],
    );
    insta::assert_json_snapshot!("kabira_vindicator_ir", &ir);
    insta::assert_json_snapshot!("kabira_vindicator_lowered", &lowered);
}

/// Leveler *body* TRIGGER, with a printed intervening-if (Plan 05b, T5a witness).
///
/// The two levelers above print only P/T, keyword and static lines inside their
/// LEVEL blocks, so neither reaches the trigger arm of the LEVEL re-parse loop
/// (`oracle.rs`, "Triggered abilities within LEVEL blocks get a HasCounters
/// condition"). Without this fixture T5a's conversion would be
/// snapshot-invisible.
///
/// Lighthouse Chronologist is chosen over the other two pool levelers with a
/// LEVEL-block trigger (Lord of Shatterskull Pass, The Fearsome Flock) because
/// its trigger is the only one that prints its own CR 603.4 intervening-if
/// ("if it's not your turn"). That makes it the witness for the composing arm
/// of the CR 711.2a/711.2b level graft — `Some(existing) => And { .. }` — and
/// not merely the `None` arm, which is the half the flat-vs-nested shape
/// question actually turns on.
#[test]
fn lighthouse_chronologist() {
    let (ir, lowered) = parse_two_layer(
        "Level up {U} ({U}: Put a level counter on this. Level up only as a sorcery.)\nLEVEL 4-6\n2/4\nLEVEL 7+\n3/5\nAt the beginning of each end step, if it's not your turn, take an extra turn after this one.",
        "Lighthouse Chronologist",
        &["Creature"],
        &["Human", "Wizard"],
    );
    insta::assert_json_snapshot!("lighthouse_chronologist_ir", &ir);
    insta::assert_json_snapshot!("lighthouse_chronologist_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Spacecraft threshold lines (Plan 05b, T2 witness)
// ---------------------------------------------------------------------------

/// Both Spacecraft static arms on one card (CR 702.184a / CR 721.2).
///
/// `2+ | Other creatures you control get +1/+1.` takes the
/// `parse_static_line` arm (`oracle_spacecraft.rs:256`); `12+ | Flying,
/// lifelink` takes the keyword-only arm (`:178`). Nothing else in the two-layer
/// corpus reaches either — Chalice of the Void carries `charge` counters but
/// prints no threshold line — so without this fixture T2's Spacecraft
/// conversion would be snapshot-invisible.
#[test]
fn lumen_class_frigate() {
    let (ir, lowered) = parse_two_layer(
        "Station (Tap another creature you control: Put charge counters equal to its power on this Spacecraft. Station only as a sorcery. It's an artifact creature at 12+.)\n2+ | Other creatures you control get +1/+1.\n12+ | Flying, lifelink",
        "Lumen-Class Frigate",
        &["Artifact"],
        &["Spacecraft"],
    );
    insta::assert_json_snapshot!("lumen_class_frigate_ir", &ir);
    insta::assert_json_snapshot!("lumen_class_frigate_lowered", &lowered);
}

/// Spacecraft threshold TRIGGER line (Plan 05b, T5a witness).
///
/// `lumen_class_frigate` above prints two static threshold lines and reaches
/// neither trigger arm. Entropic Battlecruiser prints `1+ | Whenever an
/// opponent discards a card, …`, which is the threshold-trigger arm
/// (`oracle_spacecraft.rs`), *and* an ordinary un-gated `Whenever this
/// Spacecraft attacks` trigger below the threshold block. Carrying both on one
/// card makes the fixture witness the CR 707.9a per-category trigger slot
/// ordering across the preprocessor/dispatch-loop boundary as well as the
/// threshold condition itself.
#[test]
fn entropic_battlecruiser() {
    let (ir, lowered) = parse_two_layer_with_keywords(
        "Station (Tap another creature you control: Put charge counters equal to its power on this Spacecraft. Station only as a sorcery. It's an artifact creature at 8+.)\n1+ | Whenever an opponent discards a card, they lose 3 life.\n8+ | Flying, deathtouch\nWhenever this Spacecraft attacks, each opponent discards a card. Each opponent who can't loses 3 life.",
        "Entropic Battlecruiser",
        &["station"],
        &["Artifact"],
        &["Spacecraft"],
    );
    insta::assert_json_snapshot!("entropic_battlecruiser_ir", &ir);
    insta::assert_json_snapshot!("entropic_battlecruiser_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Plan 05b T8-A3 (§5.3 remediation): U0-12 had no fixture that witnessed the
// envelope it stamps.
// ---------------------------------------------------------------------------

/// U0-12 — the CR 711.2a/711.2b LEVEL-block activated line (T8-A3 witness).
///
/// **What was unwitnessed.** The only pre-existing test over this site
/// (`oracle_tests::leveler_activated_abilities_get_level_counter_range`) asserts
/// `LevelCounterRange` presence with `.contains(…)`, which is order-insensitive,
/// and asserts nothing about the `cost` or `description` the site also stamps.
/// Dropping `LevelCounterRange` was witnessed; every other axis was not.
///
/// Guul Draz Assassin is the richest fixture in the nine-card leveler
/// population: two striations, a two-component `{B}, {T}` cost (so the CR 602.1a
/// stamp is pinned to something more than a bare `{T}`), a targeted effect, and
/// two *different* level ranges — a bounded `2-3` and an unbounded `4+` — so a
/// range that collapsed to a constant would show.
///
/// Fixture is pool-verified, not synthetic: Oracle text, `Creature` type and
/// `Vampire`/`Assassin` subtypes are verbatim from `data/card-data.json`.
#[test]
fn guul_draz_assassin_level_activated() {
    let (ir, lowered) = parse_two_layer_with_keywords(
        "Level up {1}{B} ({1}{B}: Put a level counter on this. Level up only as a sorcery.)\nLEVEL 2-3\n2/2\n{B}, {T}: Target creature gets -2/-2 until end of turn.\nLEVEL 4+\n4/4\n{B}, {T}: Target creature gets -4/-4 until end of turn.",
        "Guul Draz Assassin",
        &["level up"],
        &["Creature"],
        &["Vampire", "Assassin"],
    );
    insta::assert_json_snapshot!("guul_draz_assassin_ir", &ir);
    insta::assert_json_snapshot!("guul_draz_assassin_lowered", &lowered);
}

/// U0-12 — the first site in phase A where `ExtractManaSpendTrigger`'s guard is
/// LIVE (T8-A3 witness).
///
/// A2 established that its four keyword sites can never run that fold: no pool
/// card with those keywords lowers to a root `Effect::Mana`, so the stage
/// early-returns every time. **U0-12 is different.** Joraga Treespeaker's
/// `LEVEL 1-4` body is `{T}: Add {G}{G}.`, which lowers to a root `Effect::Mana`,
/// so the guard passes here for the first time in the tranche.
///
/// The fold's *body* still does nothing — it additionally needs a trailing "when
/// you spend this mana …" sub-ability, and no leveler card prints one — so
/// dropping the stage is still extensionally inert. Pinning the mana root is
/// what makes that distinction visible: this fixture is the one that would start
/// discriminating the moment a level striation prints a spend trigger.
///
/// Fixture is pool-verified, not synthetic: Oracle text, `Creature` type and
/// `Elf`/`Druid` subtypes are verbatim from `data/card-data.json`.
#[test]
fn joraga_treespeaker_level_mana_ability() {
    let (ir, lowered) = parse_two_layer_with_keywords(
        "Level up {1}{G} ({1}{G}: Put a level counter on this. Level up only as a sorcery.)\nLEVEL 1-4\n1/2\n{T}: Add {G}{G}.\nLEVEL 5+\n1/4\nElves you control have \"{T}: Add {G}{G}.\"",
        "Joraga Treespeaker",
        &["level up"],
        &["Creature"],
        &["Elf", "Druid"],
    );
    insta::assert_json_snapshot!("joraga_treespeaker_ir", &ir);
    insta::assert_json_snapshot!("joraga_treespeaker_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// CR 603.12 deferred rider on a top-of-library play permission
// ---------------------------------------------------------------------------

/// The `". When you do, …"` rider gap (Plan 05b, T4 witness).
///
/// The converted site emits BOTH halves of the second line: a static for the
/// top-of-library play permission and, beside it, a deliberately-honest gap
/// marker for the rider — `TriggerMode::Unknown("when you do")` with
/// `execute: None`, so coverage shows the gap instead of an approximated and
/// rules-incorrect `PlayCard` trigger.
///
/// This fixture exists because T4's churn is otherwise ZERO: none of the 28
/// corpus cards carrying a trigger reaches this site, which would make the
/// tranche's byte gate vacuous. It is the non-vacuity proof — the `_ir`
/// snapshot must show a `Trigger` node rather than the pre-lowered variant on
/// line 1, and `_lowered` must match what the old path produced.
#[test]
fn the_fourth_doctor() {
    let (ir, lowered) = parse_two_layer(
        "You may look at the top card of your library any time.\nWould You Like A...? — Once each turn, you may play a historic land or cast a historic spell from the top of your library. When you do, create a Food token. (Artifacts, legendaries, and Sagas are historic.)",
        "The Fourth Doctor",
        &["Creature"],
        &["Time Lord", "Doctor"],
    );
    insta::assert_json_snapshot!("the_fourth_doctor_ir", &ir);
    insta::assert_json_snapshot!("the_fourth_doctor_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Adventure
// ---------------------------------------------------------------------------

#[test]
fn bonecrusher_giant() {
    let (ir, lowered) = parse_two_layer(
        "Whenever this creature becomes the target of a spell, this creature deals 2 damage to that spell's controller.",
        "Bonecrusher Giant",
        &["Creature"],
        &["Giant"],
    );
    insta::assert_json_snapshot!("bonecrusher_giant_ir", &ir);
    insta::assert_json_snapshot!("bonecrusher_giant_lowered", &lowered);
}

#[test]
fn brazen_borrower() {
    let (ir, lowered) = parse_two_layer(
        "Flash\nFlying\nThis creature can block only creatures with flying.",
        "Brazen Borrower",
        &["Creature"],
        &["Faerie", "Rogue"],
    );
    insta::assert_json_snapshot!("brazen_borrower_ir", &ir);
    insta::assert_json_snapshot!("brazen_borrower_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Kicker
// ---------------------------------------------------------------------------

#[test]
fn vines_of_vastwood() {
    let (ir, lowered) = parse_two_layer(
        "Kicker {G} (You may pay an additional {G} as you cast this spell.)\nTarget creature can't be the target of spells or abilities your opponents control this turn. If this spell was kicked, that creature gets +4/+4 until end of turn.",
        "Vines of Vastwood",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("vines_of_vastwood_ir", &ir);
    insta::assert_json_snapshot!("vines_of_vastwood_lowered", &lowered);
}

#[test]
fn reckless_bushwhacker() {
    let (ir, lowered) = parse_two_layer(
        "Surge {1}{R} (You may cast this spell for its surge cost if you or a teammate has cast another spell this turn.)\nHaste\nWhen this creature enters, if its surge cost was paid, other creatures you control get +1/+0 and gain haste until end of turn.",
        "Reckless Bushwhacker",
        &["Creature"],
        &["Goblin", "Warrior"],
    );
    insta::assert_json_snapshot!("reckless_bushwhacker_ir", &ir);
    insta::assert_json_snapshot!("reckless_bushwhacker_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Channel
// ---------------------------------------------------------------------------

#[test]
fn boseiju_who_endures() {
    let (ir, lowered) = parse_two_layer(
        "{T}: Add {G}.\nChannel \u{2014} {1}{G}, Discard this card: Destroy target artifact, enchantment, or nonbasic land an opponent controls. That player may search their library for a land card with a basic land type, put it onto the battlefield, then shuffle. This ability costs {1} less to activate for each legendary creature you control.",
        "Boseiju, Who Endures",
        &["Land"],
        &[],
    );
    insta::assert_json_snapshot!("boseiju_who_endures_ir", &ir);
    insta::assert_json_snapshot!("boseiju_who_endures_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Enchantments with multiple ability types
// ---------------------------------------------------------------------------

#[test]
fn conclave_mentor() {
    let (ir, lowered) = parse_two_layer(
        "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on that creature instead.\nWhen this creature dies, you gain life equal to its power.",
        "Conclave Mentor",
        &["Creature"],
        &["Centaur", "Cleric"],
    );
    insta::assert_json_snapshot!("conclave_mentor_ir", &ir);
    insta::assert_json_snapshot!("conclave_mentor_lowered", &lowered);
}

#[test]
fn luminarch_aspirant() {
    let (ir, lowered) = parse_two_layer(
        "At the beginning of combat on your turn, put a +1/+1 counter on target creature you control.",
        "Luminarch Aspirant",
        &["Creature"],
        &["Human", "Cleric"],
    );
    insta::assert_json_snapshot!("luminarch_aspirant_ir", &ir);
    insta::assert_json_snapshot!("luminarch_aspirant_lowered", &lowered);
}

#[test]
fn mishra_eminent_one() {
    let (ir, lowered) = parse_two_layer(
        "At the beginning of combat on your turn, create a token that's a copy of target noncreature artifact you control, except its name is Mishra's Warform and it's a 4/4 Construct artifact creature in addition to its other types. It gains haste until end of turn. Sacrifice it at the beginning of the next end step.",
        "Mishra, Eminent One",
        &["Legendary", "Artifact", "Creature"],
        &["Human", "Artificer"],
    );
    insta::assert_json_snapshot!("mishra_eminent_one_ir", &ir);
    insta::assert_json_snapshot!("mishra_eminent_one_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Ability words (Landfall, Prowess, Evolve)
// ---------------------------------------------------------------------------

#[test]
fn tireless_tracker() {
    let (ir, lowered) = parse_two_layer(
        "Landfall \u{2014} Whenever a land you control enters, investigate. (Create a Clue token. It's an artifact with \"{2}, Sacrifice this token: Draw a card.\")\nWhenever you sacrifice a Clue, put a +1/+1 counter on this creature.",
        "Tireless Tracker",
        &["Creature"],
        &["Human", "Scout"],
    );
    insta::assert_json_snapshot!("tireless_tracker_ir", &ir);
    insta::assert_json_snapshot!("tireless_tracker_lowered", &lowered);
}

#[test]
fn monastery_swiftspear() {
    let (ir, lowered) = parse_two_layer(
        "Haste\nProwess (Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn.)",
        "Monastery Swiftspear",
        &["Creature"],
        &["Human", "Monk"],
    );
    insta::assert_json_snapshot!("monastery_swiftspear_ir", &ir);
    insta::assert_json_snapshot!("monastery_swiftspear_lowered", &lowered);
}

#[test]
fn experiment_one() {
    let (ir, lowered) = parse_two_layer(
        "Evolve (Whenever a creature you control enters, if that creature has greater power or toughness than this creature, put a +1/+1 counter on this creature.)\nRemove two +1/+1 counters from this creature: Regenerate it. (The next time this creature would be destroyed this turn, instead tap it, remove it from combat, and heal all damage on it.)",
        "Experiment One",
        &["Creature"],
        &["Human", "Ooze"],
    );
    insta::assert_json_snapshot!("experiment_one_ir", &ir);
    insta::assert_json_snapshot!("experiment_one_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Deeply nested / multi-clause spells
// ---------------------------------------------------------------------------

#[test]
fn swords_to_plowshares() {
    let (ir, lowered) = parse_two_layer(
        "Exile target creature. Its controller gains life equal to its power.",
        "Swords to Plowshares",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("swords_to_plowshares_ir", &ir);
    insta::assert_json_snapshot!("swords_to_plowshares_lowered", &lowered);
}

#[test]
fn kroxa_titan_of_deaths_hunger() {
    let (ir, lowered) = parse_two_layer(
        "When Kroxa enters, sacrifice it unless it escaped.\nWhenever Kroxa enters or attacks, each opponent discards a card, then each opponent who didn't discard a nonland card this way loses 3 life.\nEscape\u{2014}{B}{B}{R}{R}, Exile five other cards from your graveyard. (You may cast this card from your graveyard for its escape cost.)",
        "Kroxa, Titan of Death's Hunger",
        &["Creature"],
        &["Elder", "Giant"],
    );
    insta::assert_json_snapshot!("kroxa_titan_ir", &ir);
    insta::assert_json_snapshot!("kroxa_titan_lowered", &lowered);
}

#[test]
fn snapcaster_mage() {
    let (ir, lowered) = parse_two_layer(
        "Flash\nWhen this creature enters, target instant or sorcery card in your graveyard gains flashback until end of turn. The flashback cost is equal to its mana cost. (You may cast that card from your graveyard for its flashback cost. Then exile it.)",
        "Snapcaster Mage",
        &["Creature"],
        &["Human", "Wizard"],
    );
    insta::assert_json_snapshot!("snapcaster_mage_ir", &ir);
    insta::assert_json_snapshot!("snapcaster_mage_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Damage sub_ability riders (U5-M2 Absorb parity: die-exile / can't-regenerate)
// ---------------------------------------------------------------------------

// CR 608.2c + CR 701.19c: unconditional "can't be regenerated" rider on a
// separate sentence after a damage clause
// (ClauseDisposition::Absorb { kind: CantBeRegenerated }). Verified verbatim
// against Scryfall.
#[test]
fn incinerate() {
    let (ir, lowered) = parse_two_layer(
        "Incinerate deals 3 damage to any target. A creature dealt damage this way can't be regenerated this turn.",
        "Incinerate",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("incinerate_ir", &ir);
    insta::assert_json_snapshot!("incinerate_lowered", &lowered);
}

// CR 614.1a + CR 514.2: standalone "if [it] would die this turn, exile it
// instead" die-exile rider on a separate sentence after a damage clause
// (ClauseDisposition::Absorb { kind: DieExile }).
// Verified verbatim against Scryfall (includes the printed Devoid keyword line).
#[test]
fn touch_of_the_void() {
    let (ir, lowered) = parse_two_layer(
        "Devoid (This card has no color.)\nTouch of the Void deals 3 damage to any target. If a creature dealt damage this way would die this turn, exile it instead.",
        "Touch of the Void",
        &["Sorcery"],
        &[],
    );
    insta::assert_json_snapshot!("touch_of_the_void_ir", &ir);
    insta::assert_json_snapshot!("touch_of_the_void_lowered", &lowered);
}

// CR 109.2 + CR 608.2c: the conditional two-rider form ("If it's a creature, it
// can't be regenerated this turn, and if it would die this turn, exile it
// instead.") emits BOTH Absorb kinds from the conditional-regen block —
// CantBeRegenerated then DieExile, each stamped with the creature-gate
// condition. Verified verbatim against Scryfall.
#[test]
fn carbonize() {
    let (ir, lowered) = parse_two_layer(
        "Carbonize deals 3 damage to any target. If it's a creature, it can't be regenerated this turn, and if it would die this turn, exile it instead.",
        "Carbonize",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("carbonize_ir", &ir);
    insta::assert_json_snapshot!("carbonize_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// "Otherwise" else-branches (U5-M2 BranchOtherwise parity)
// ---------------------------------------------------------------------------
// All three exercise the Bound kind (prior conditional / opponent-may head
// present at parse time); Fallback has no real corpus card (0/568 fixture
// cards). Oracle text verified verbatim against Scryfall.

// CR 608.2c + CR 205.3a: Bound → attach-to-conditional + self-ref rebind
// (`definition_targets_self_source` → `rewrite_else_parent_target_to_self_ref`,
// so the else "it" binds to the source rather than an empty target list).
#[test]
fn repeat_offender() {
    let (ir, lowered) = parse_two_layer(
        "{2}{B}: If this creature is suspected, put a +1/+1 counter on it. Otherwise, suspect it. (A suspected creature has menace and can't block.)",
        "Repeat Offender",
        &["Creature"],
        &["Human", "Assassin"],
    );
    insta::assert_json_snapshot!("repeat_offender_ir", &ir);
    insta::assert_json_snapshot!("repeat_offender_lowered", &lowered);
}

// CR 608.2c: Bound → attach-to-conditional + event-context "that much" rebind
// (`rewrite_else_event_context_to_stable`, so the else's "that much" reads the
// if-branch's stable magnitude instead of a per-instruction 0).
#[test]
fn caustic_bronco() {
    let (ir, lowered) = parse_two_layer(
        "Whenever this creature attacks, reveal the top card of your library and put it into your hand. You lose life equal to that card's mana value if this creature isn't saddled. Otherwise, each opponent loses that much life.\nSaddle 3 (Tap any number of other creatures you control with total power 3 or more: This Mount becomes saddled until end of turn. Saddle only as a sorcery.)",
        "Caustic Bronco",
        &["Creature"],
        &["Snake", "Horse", "Mount"],
    );
    insta::assert_json_snapshot!("caustic_bronco_ir", &ir);
    insta::assert_json_snapshot!("caustic_bronco_lowered", &lowered);
}

// CR 608.2d + CR 101.4: Bound → opponent-may reward branch (no explicit
// condition, but the "any player may" head sets `opponent_may_scope`, so
// `has_optional_may_head` routes it Bound; the handler's `!attached` fallback
// synthesizes the `Not(OptionalEffectPerformed)`-gated reward on the may-head).
// The "If no one does, …" connector is one of the recognized otherwise forms.
#[test]
fn browbeat() {
    let (ir, lowered) = parse_two_layer(
        "Any player may have Browbeat deal 5 damage to them. If no one does, target player draws three cards.",
        "Browbeat",
        &["Sorcery"],
        &[],
    );
    insta::assert_json_snapshot!("browbeat_ir", &ir);
    insta::assert_json_snapshot!("browbeat_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Per-keyword replication (U5-M2 ReplicatePerKeyword parity)
// ---------------------------------------------------------------------------
// Oracle text verified verbatim against Scryfall.

// CR 702: StaticGrant — "The same is true for <keywords>." replicates the
// antecedent static keyword-grant clause once per listed keyword, swapping the
// keyword in both the grant and its gating condition (Odric, Lunarch Marshal).
#[test]
fn odric_lunarch_marshal() {
    let (ir, lowered) = parse_two_layer(
        "At the beginning of each combat, creatures you control gain first strike until end of turn if a creature you control has first strike. The same is true for flying, deathtouch, double strike, haste, hexproof, indestructible, lifelink, menace, reach, skulk, trample, and vigilance.",
        "Odric, Lunarch Marshal",
        &["Creature"],
        &["Human", "Soldier"],
    );
    insta::assert_json_snapshot!("odric_lunarch_marshal_ir", &ir);
    insta::assert_json_snapshot!("odric_lunarch_marshal_lowered", &lowered);
}

// CR 608.2c: CounterPlacement — "Repeat this process for <keywords>." replicates
// the antecedent conditional keyword-counter clause once per listed keyword,
// swapping the keyword in both the placed counter and the graveyard-keyword gate
// (Kathril, Aspect Warper).
#[test]
fn kathril_aspect_warper() {
    let (ir, lowered) = parse_two_layer(
        "When Kathril enters, put a flying counter on any creature you control if a creature card in your graveyard has flying. Repeat this process for first strike, double strike, deathtouch, hexproof, indestructible, lifelink, menace, reach, trample, and vigilance. Then put a +1/+1 counter on Kathril for each counter put on a creature this way.",
        "Kathril, Aspect Warper",
        &["Creature"],
        &["Nightmare", "Insect"],
    );
    insta::assert_json_snapshot!("kathril_aspect_warper_ir", &ir);
    insta::assert_json_snapshot!("kathril_aspect_warper_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Prior-def modifiers (U5-M2 ModifyPrior parity)
// ---------------------------------------------------------------------------
// Oracle text verified verbatim against Scryfall. AltCost + ManaRetention have
// real cards below; the third ModifyPrior kind (EntersTappedAttacking) has no
// card in the 568-card fixture and a complex pop+patch body — it is covered by a
// direct handler unit test in oracle_trigger_tests.rs instead.

// CR 118.9 + CR 119.4: AltCost — "pay <cost> rather than paying its mana cost."
// folds an `alt_ability_cost` onto the prior CastFromZone play grant (Nashi,
// Moon Sage's Scion).
#[test]
fn nashi_moon_sages_scion() {
    let (ir, lowered) = parse_two_layer(
        "Ninjutsu {3}{B} ({3}{B}, Return an unblocked attacker you control to hand: Put this card onto the battlefield from your hand tapped and attacking.)\nWhenever Nashi deals combat damage to a player, exile the top card of each player's library. Until end of turn, you may play one of those cards. If you cast a spell this way, pay life equal to its mana value rather than paying its mana cost.",
        "Nashi, Moon Sage's Scion",
        &["Creature"],
        &["Rat", "Ninja"],
    );
    insta::assert_json_snapshot!("nashi_moon_sages_scion_ir", &ir);
    insta::assert_json_snapshot!("nashi_moon_sages_scion_lowered", &lowered);
}

// CR 106.4: ManaRetention — "you don't lose this mana as steps and phases end."
// folds a mana-retention expiry onto the prior mana-production effect (Karn,
// Legacy Reforged).
#[test]
fn karn_legacy_reforged() {
    let (ir, lowered) = parse_two_layer(
        "Karn's power and toughness are each equal to the greatest mana value among artifacts you control.\nAt the beginning of your upkeep, add {C} for each artifact you control. This mana can't be spent to cast nonartifact spells. Until end of turn, you don't lose this mana as steps and phases end.",
        "Karn, Legacy Reforged",
        &["Artifact", "Creature"],
        &["Golem"],
    );
    insta::assert_json_snapshot!("karn_legacy_reforged_ir", &ir);
    insta::assert_json_snapshot!("karn_legacy_reforged_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Meaning-replacement overrides (U5-M2 ReplaceMeaning parity)
// ---------------------------------------------------------------------------
// All three kinds have real cards in the 568-card fixture (DigAlt 2, Instead 24,
// KeywordOverride 1). Oracle text verified verbatim against Scryfall.

// CR 608.2c: DigAlt — "you may instead <alternative dig disposition>" pops the
// prior dig def and wraps the alternative with the prior as its `else_ability`
// (Follow the Lumarets). "Infusion —" is an ability word (stripped like Landfall).
#[test]
fn follow_the_lumarets() {
    let (ir, lowered) = parse_two_layer(
        "Infusion — Look at the top four cards of your library. You may reveal a creature or land card from among them and put it into your hand. If you gained life this turn, you may instead reveal two creature and/or land cards from among them and put them into your hand. Put the rest on the bottom of your library in a random order.",
        "Follow the Lumarets",
        &["Sorcery"],
        &[],
    );
    assert!(matches!(ir.items[0].node, OracleNodeIr::Spell(_)));
    assert_eq!(
        lowered.abilities.len(),
        1,
        "the Dig override must bind through the document relation"
    );
    assert!(lowered.abilities[0].else_ability.is_some());
    insta::assert_json_snapshot!("follow_the_lumarets_ir", &ir);
    insta::assert_json_snapshot!("follow_the_lumarets_lowered", &lowered);
}

/// CR 614.6 + CR 614.15: an override whose condition cannot lower stays on the
/// `instead_override` floor; it must never become an independent second spell.
#[test]
fn priority_nine_unbindable_conditioned_replacement_stays_honest() {
    let (ir, lowered) = parse_two_layer(
        "Draw a card.\nMystery — Draw two cards instead if the cracks in this artifact's art are completely covered.",
        "Unbindable Override Fixture",
        &["Sorcery"],
        &[],
    );
    assert!(matches!(ir.items[0].node, OracleNodeIr::Spell(_)));
    assert!(matches!(ir.items[1].node, OracleNodeIr::Spell(_)));
    assert_eq!(lowered.abilities.len(), 2);
    assert!(matches!(
        lowered.abilities[1].effect.as_ref(),
        Effect::Unimplemented { name, .. } if name == "instead_override"
    ));
    assert!(lowered.abilities[1].condition.is_none());
}

fn assert_unbindable_override(def: &crate::types::ability::AbilityDefinition) {
    assert!(matches!(
        def.effect.as_ref(),
        Effect::Unimplemented { name, .. } if name == "instead_override"
    ));
    assert!(def.sub_ability.is_none());
    assert!(def.else_ability.is_none());
}

fn roll_die_result_count(def: &crate::types::ability::AbilityDefinition) -> Option<usize> {
    match def.effect.as_ref() {
        Effect::RollDie { results, .. } => Some(results.len()),
        _ => def
            .sub_ability
            .as_deref()
            .and_then(roll_die_result_count)
            .or_else(|| def.else_ability.as_deref().and_then(roll_die_result_count)),
    }
}

/// CR 706.3b: recognized contiguous result branches stay with an inline roll
/// even when the ability's paragraph has instructions both before and after it.
fn assert_inline_die_table(
    oracle_text: &str,
    card_name: &str,
    types: &[&str],
    expected_recognized_results: usize,
) {
    let (_, lowered) = parse_two_layer(oracle_text, card_name, types, &[]);
    assert_eq!(
        lowered.abilities.len(),
        1,
        "{card_name} must retain its result rows on the printed ability"
    );
    assert_eq!(
        roll_die_result_count(&lowered.abilities[0]),
        Some(expected_recognized_results),
        "{card_name} must retain its recognized result branches on the inline roll"
    );
}

#[test]
fn laezels_acrobatics_inline_die_table_is_owned_by_nonterminal_roll() {
    assert_inline_die_table(
        "Exile all nontoken creatures you control, then roll a d20.\n1—9 | Return those cards to the battlefield under their owner's control at the beginning of the next end step.\n10—20 | Return those cards to the battlefield under their owner's control, then exile them again. Return those cards to the battlefield under their owner's control at the beginning of the next end step.",
        "Lae'zel's Acrobatics",
        &["Instant"],
        1,
    );
}

#[test]
fn overwhelming_encounter_inline_die_table_is_owned_by_nonterminal_roll() {
    assert_inline_die_table(
        "Creatures you control gain vigilance and trample until end of turn. Roll a d20.\n1—9 | Creatures you control get +2/+2 until end of turn.\n10—19 | Put two +1/+1 counters on each creature you control.\n20 | Put four +1/+1 counters on each creature you control.",
        "Overwhelming Encounter",
        &["Sorcery"],
        2,
    );
}

#[test]
fn deck_of_many_things_inline_die_table_is_owned_by_modified_roll() {
    assert_inline_die_table(
        "{2}, {T}: Roll a d20 and subtract the number of cards in your hand. If the result is 0 or less, discard your hand.\n1—9 | Return a card at random from your graveyard to your hand.\n10—19 | Draw two cards.\n20 | Put a creature card from any graveyard onto the battlefield under your control. When that creature dies, its owner loses the game.",
        "The Deck of Many Things",
        &["Artifact"],
        3,
    );
}

#[test]
fn wand_of_wonder_inline_die_table_is_owned_by_roll_with_later_instructions() {
    assert_inline_die_table(
        "{4}, {T}: Roll a d20. Each opponent exiles cards from the top of their library until they exile an instant or sorcery card, then shuffles the rest into their library. You may cast up to X instant and/or sorcery spells from among cards exiled this way without paying their mana costs.\n1—9 | X is one.\n10—19 | X is two.\n20 | X is three.",
        "Wand of Wonder",
        &["Artifact"],
        3,
    );
}

#[test]
fn inline_roll_without_immediate_result_row_does_not_consume_following_text() {
    let (ir, lowered) = parse_two_layer(
        "Draw a card, then roll a d20.\nFlying\n1—20 | Draw two cards.",
        "Inline Roll Without Table Fixture",
        &["Sorcery"],
        &[],
    );
    assert!(!ir.items.is_empty());
    assert!(!lowered.abilities.is_empty());
    assert_eq!(roll_die_result_count(&lowered.abilities[0]), Some(0));
}

#[test]
fn roll_text_without_typed_roll_die_does_not_consume_result_rows() {
    let (ir, lowered) = parse_two_layer(
        "Draw a card, then roll a dword.\n1—20 | Draw two cards.",
        "Unparsed Roll Text Fixture",
        &["Sorcery"],
        &[],
    );
    assert_eq!(ir.items.len(), 2);
    assert_eq!(lowered.abilities.len(), 2);
    assert_eq!(roll_die_result_count(&lowered.abilities[0]), None);
}

/// CR 614.6 + CR 614.15: the native override floor retains the root clause's
/// resolution metadata while making the unsupported replacement explicit.
#[test]
fn caravan_vigil_unbindable_override_retains_optional() {
    let (_, lowered) = parse_two_layer(
        "Search your library for a basic land card, reveal it, put it into your hand, then shuffle.\nMorbid — You may put that card onto the battlefield instead of putting it into your hand if a creature died this turn.",
        "Caravan Vigil",
        &["Sorcery"],
        &[],
    );
    let override_def = &lowered.abilities[1];
    assert_unbindable_override(override_def);
    assert!(override_def.optional);
}

/// CR 614.6 + CR 614.15: partial cross-line replacements preserve a parsed
/// optional root even when the replacement cannot bind safely.
#[test]
fn talent_of_the_telepath_unbindable_override_retains_optional() {
    let (_, lowered) = parse_two_layer(
        "Target opponent reveals the top seven cards of their library. You may cast an instant or sorcery spell from among them without paying its mana cost. Then that player puts the rest into their graveyard.\nSpell mastery — If there are two or more instant and/or sorcery cards in your graveyard, you may cast up to two instant and/or sorcery spells from among the revealed cards instead of one.",
        "Talent of the Telepath",
        &["Sorcery"],
        &[],
    );
    let override_def = &lowered.abilities[1];
    assert_unbindable_override(override_def);
    assert!(override_def.optional);
}

/// CR 614.6 + CR 614.15: an unbindable partial replacement keeps the original
/// root's resolution-time selection stamp instead of inferring it from the floor.
#[test]
fn see_the_unwritten_unbindable_override_retains_target_choice_timing() {
    let (_, lowered) = parse_two_layer(
        "Reveal the top eight cards of your library. You may put a creature card from among them onto the battlefield. Put the rest into your graveyard.\nFerocious — If you control a creature with power 4 or greater, you may put two creature cards onto the battlefield instead of one.",
        "See the Unwritten",
        &["Sorcery"],
        &[],
    );
    let override_def = &lowered.abilities[1];
    assert_unbindable_override(override_def);
    assert_eq!(
        override_def.target_choice_timing,
        TargetChoiceTiming::Resolution
    );
}

// CR 614.1a + CR 608.2c: Instead — the multi-clause Cow-swap. Clause 1 ("gain
// control … until end of turn") is the root/swap target; the "… instead" override
// carries the `ConditionInstead`, and the TAIL clauses ("Untap that creature. It
// gains haste …") are stashed in the override's `else_ability` (Evil's Thrall).
#[test]
fn evils_thrall() {
    let (ir, lowered) = parse_two_layer(
        "Gain control of target creature until end of turn. If you control a Villain with greater mana value than that creature, gain control of that creature until the end of your next turn instead. Untap that creature. It gains haste until end of turn.",
        "Evil's Thrall",
        &["Sorcery"],
        &[],
    );
    insta::assert_json_snapshot!("evils_thrall_ir", &ir);
    insta::assert_json_snapshot!("evils_thrall_lowered", &lowered);
}

// CR 608.2c: KeywordOverride — a "TargetHasKeywordInstead"-conditioned clause
// builds its def from the parsed effect + condition and attaches as the prior
// def's `sub_ability` (Conformer Shuriken's granted attack trigger).
#[test]
fn conformer_shuriken() {
    let (ir, lowered) = parse_two_layer(
        "Equipped creature has \"Whenever this creature attacks, tap target creature defending player controls. If that creature has greater power than this creature, put a number of +1/+1 counters on this creature equal to the difference.\"\nEquip {2}",
        "Conformer Shuriken",
        &["Artifact"],
        &["Equipment"],
    );
    insta::assert_json_snapshot!("conformer_shuriken_ir", &ir);
    insta::assert_json_snapshot!("conformer_shuriken_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Search-fold + drawn-this-turn follow-up (U5-M2 capstone parity)
// ---------------------------------------------------------------------------
// The last two special-clause markers, now typed dispositions. Oracle text
// verified verbatim against Scryfall. Reach-guards asserting these texts really
// land on the new dispositions live in `oracle_effect::tests`
// (`*_reaches_fold_search_into_else`, `sylvan_library_reaches_drawn_this_turn_followup`) —
// `ClauseDisposition` is never serialized, so a snapshot alone cannot show which
// arm ran.

// CR 608.2c + CR 601.2b: FoldSearchIntoElse — an "instead, search your library …"
// clause whose additional cost was paid (CR 601.2b) is later text modifying the
// meaning of the earlier search (CR 608.2c). It builds its own def and folds the
// PRIOR search's trailing search-destination `ChangeZone` into its `else_ability`,
// then applies its OWN intrinsic `SearchDestination` continuation. Kicker variant.
#[test]
fn aangs_journey() {
    let (ir, lowered) = parse_two_layer(
        "Kicker {2} (You may pay an additional {2} as you cast this spell.)\nSearch your library for a basic land card. If this spell was kicked, instead search your library for a basic land card and a Shrine card. Reveal those cards, put them into your hand, then shuffle.\nYou gain 2 life.",
        "Aang's Journey",
        &["Sorcery"],
        &["Lesson"],
    );
    insta::assert_json_snapshot!("aangs_journey_ir", &ir);
    insta::assert_json_snapshot!("aangs_journey_lowered", &lowered);
}

// CR 608.2c + CR 601.2b: FoldSearchIntoElse, second card of the class — the cost
// is `collect evidence` rather than kicker, and the search reveals (the intrinsic
// carries `reveal: true`). Same disposition, different cost + intrinsic payload:
// this is the class, not the card.
#[test]
fn analyze_the_pollen() {
    let (ir, lowered) = parse_two_layer(
        "As an additional cost to cast this spell, you may collect evidence 8. (Exile cards with total mana value 8 or greater from your graveyard.)\nSearch your library for a basic land card. If evidence was collected, instead search your library for a creature or land card. Reveal that card, put it into your hand, then shuffle.",
        "Analyze the Pollen",
        &["Sorcery"],
        &[],
    );
    insta::assert_json_snapshot!("analyze_the_pollen_ir", &ir);
    insta::assert_json_snapshot!("analyze_the_pollen_lowered", &lowered);
}

// DrawnThisTurnFollowup — "For each of those cards, pay N life or put the card on
// top of your library" sets the life payment on the prior
// `ChooseDrawnThisTurnPayOrTopdeck` and emits no def of its own (Sylvan Library).
// NOTE: this is the only card of its class and it is NOT in the 568-card fixture
// corpus, so the payment write is additionally pinned by a direct handler test
// (`drawn_this_turn_followup_overwrites_prior_life_payment`) using a NON-default
// payment — Sylvan Library's parsed default is already 4, so asserting 4 here
// would be vacuous.
#[test]
fn sylvan_library() {
    let (ir, lowered) = parse_two_layer(
        "At the beginning of your draw step, you may draw two additional cards. If you do, choose two cards in your hand drawn this turn. For each of those cards, pay 4 life or put the card on top of your library.",
        "Sylvan Library",
        &["Enchantment"],
        &[],
    );
    insta::assert_json_snapshot!("sylvan_library_ir", &ir);
    insta::assert_json_snapshot!("sylvan_library_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Triggers (various patterns)
// ---------------------------------------------------------------------------

#[test]
fn goblin_guide() {
    let (ir, lowered) = parse_two_layer(
        "Haste\nWhenever this creature attacks, defending player reveals the top card of their library. If it's a land card, that player puts it into their hand.",
        "Goblin Guide",
        &["Creature"],
        &["Goblin", "Scout"],
    );
    insta::assert_json_snapshot!("goblin_guide_ir", &ir);
    insta::assert_json_snapshot!("goblin_guide_lowered", &lowered);
}

#[test]
fn young_pyromancer() {
    let (ir, lowered) = parse_two_layer(
        "Whenever you cast an instant or sorcery spell, create a 1/1 red Elemental creature token.",
        "Young Pyromancer",
        &["Creature"],
        &["Human", "Shaman"],
    );
    insta::assert_json_snapshot!("young_pyromancer_ir", &ir);
    insta::assert_json_snapshot!("young_pyromancer_lowered", &lowered);
}

#[test]
fn jaws_of_defeat() {
    let (ir, lowered) = parse_two_layer(
        "Whenever a creature you control enters, target opponent loses life equal to the difference between that creature's power and its toughness.",
        "Jaws of Defeat",
        &["Enchantment"],
        &[],
    );
    insta::assert_json_snapshot!("jaws_of_defeat_ir", &ir);
    insta::assert_json_snapshot!("jaws_of_defeat_lowered", &lowered);
}

#[test]
fn dark_confidant() {
    let (ir, lowered) = parse_two_layer(
        "At the beginning of your upkeep, reveal the top card of your library and put that card into your hand. You lose life equal to its mana value.",
        "Dark Confidant",
        &["Creature"],
        &["Human", "Wizard"],
    );
    insta::assert_json_snapshot!("dark_confidant_ir", &ir);
    insta::assert_json_snapshot!("dark_confidant_lowered", &lowered);
}

#[test]
fn fevered_visions() {
    let (ir, lowered) = parse_two_layer(
        "At the beginning of each player's end step, that player draws a card. If the player is your opponent and has four or more cards in hand, this enchantment deals 2 damage to that player.",
        "Fevered Visions",
        &["Enchantment"],
        &[],
    );
    insta::assert_json_snapshot!("fevered_visions_ir", &ir);
    insta::assert_json_snapshot!("fevered_visions_lowered", &lowered);
}

#[test]
fn eidolon_of_the_great_revel() {
    let (ir, lowered) = parse_two_layer(
        "Whenever a player casts a spell with mana value 3 or less, this creature deals 2 damage to that player.",
        "Eidolon of the Great Revel",
        &["Creature"],
        &["Spirit"],
    );
    insta::assert_json_snapshot!("eidolon_of_the_great_revel_ir", &ir);
    insta::assert_json_snapshot!("eidolon_of_the_great_revel_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Static abilities
// ---------------------------------------------------------------------------

#[test]
fn leonin_arbiter() {
    let (ir, lowered) = parse_two_layer(
        "Players can't search libraries. Any player may pay {2} for that player to ignore this effect until end of turn.",
        "Leonin Arbiter",
        &["Creature"],
        &["Cat", "Cleric"],
    );
    insta::assert_json_snapshot!("leonin_arbiter_ir", &ir);
    insta::assert_json_snapshot!("leonin_arbiter_lowered", &lowered);
}

#[test]
fn lovestruck_beast() {
    let (ir, lowered) = parse_two_layer(
        "This creature can't attack unless you control a 1/1 creature.",
        "Lovestruck Beast",
        &["Creature"],
        &["Beast", "Noble"],
    );
    insta::assert_json_snapshot!("lovestruck_beast_ir", &ir);
    insta::assert_json_snapshot!("lovestruck_beast_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// CDA (Characteristic-defining ability)
// ---------------------------------------------------------------------------

#[test]
fn tarmogoyf() {
    let (ir, lowered) = parse_two_layer(
        "Tarmogoyf's power is equal to the number of card types among cards in all graveyards and its toughness is equal to that number plus 1.",
        "Tarmogoyf",
        &["Creature"],
        &["Lhurgoyf"],
    );
    insta::assert_json_snapshot!("tarmogoyf_ir", &ir);
    insta::assert_json_snapshot!("tarmogoyf_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Equipment with living weapon
// ---------------------------------------------------------------------------

#[test]
fn batterskull() {
    let (ir, lowered) = parse_two_layer(
        "Living weapon (When this Equipment enters, create a 0/0 black Phyrexian Germ creature token, then attach this to it.)\nEquipped creature gets +4/+4 and has vigilance and lifelink.\n{3}: Return this Equipment to its owner's hand.\nEquip {5}",
        "Batterskull",
        &["Artifact"],
        &["Equipment"],
    );
    insta::assert_json_snapshot!("batterskull_ir", &ir);
    insta::assert_json_snapshot!("batterskull_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// ETB with counters / X spells
// ---------------------------------------------------------------------------

#[test]
fn walking_ballista() {
    let (ir, lowered) = parse_two_layer(
        "This creature enters with X +1/+1 counters on it.\n{4}: Put a +1/+1 counter on this creature.\nRemove a +1/+1 counter from this creature: It deals 1 damage to any target.",
        "Walking Ballista",
        &["Artifact", "Creature"],
        &["Construct"],
    );
    insta::assert_json_snapshot!("walking_ballista_ir", &ir);
    insta::assert_json_snapshot!("walking_ballista_lowered", &lowered);
}

#[test]
fn chalice_of_the_void() {
    let (ir, lowered) = parse_two_layer(
        "This artifact enters with X charge counters on it.\nWhenever a player casts a spell with mana value equal to the number of charge counters on this artifact, counter that spell.",
        "Chalice of the Void",
        &["Artifact"],
        &[],
    );
    insta::assert_json_snapshot!("chalice_of_the_void_ir", &ir);
    insta::assert_json_snapshot!("chalice_of_the_void_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Phyrexian mana
// ---------------------------------------------------------------------------

#[test]
fn dismember() {
    let (ir, lowered) = parse_two_layer(
        "({B/P} can be paid with either {B} or 2 life.)\nTarget creature gets -5/-5 until end of turn.",
        "Dismember",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("dismember_ir", &ir);
    insta::assert_json_snapshot!("dismember_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Changeling
// ---------------------------------------------------------------------------

#[test]
fn changeling_outcast() {
    let (ir, lowered) = parse_two_layer(
        "Changeling (This card is every creature type.)\nThis creature can't block and can't be blocked.",
        "Changeling Outcast",
        &["Creature"],
        &["Shapeshifter"],
    );
    insta::assert_json_snapshot!("changeling_outcast_ir", &ir);
    insta::assert_json_snapshot!("changeling_outcast_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn edge_case_empty_oracle_text() {
    let (ir, lowered) = parse_two_layer("", "Grizzly Bears", &["Creature"], &["Bear"]);
    insta::assert_json_snapshot!("edge_case_empty_ir", &ir);
    insta::assert_json_snapshot!("edge_case_empty_lowered", &lowered);
}

#[test]
fn edge_case_reminder_text_only() {
    let (ir, lowered) = parse_two_layer("({T}: Add {R}.)", "Mountain", &["Land"], &["Mountain"]);
    insta::assert_json_snapshot!("edge_case_reminder_only_ir", &ir);
    insta::assert_json_snapshot!("edge_case_reminder_only_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Mana abilities (multi-color)
// ---------------------------------------------------------------------------

#[test]
fn birds_of_paradise() {
    let (ir, lowered) = parse_two_layer(
        "Flying\n{T}: Add one mana of any color.",
        "Birds of Paradise",
        &["Creature"],
        &["Bird"],
    );
    insta::assert_json_snapshot!("birds_of_paradise_ir", &ir);
    insta::assert_json_snapshot!("birds_of_paradise_lowered", &lowered);
}

#[test]
fn manamorphose() {
    let (ir, lowered) = parse_two_layer(
        "Add two mana in any combination of colors.\nDraw a card.",
        "Manamorphose",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("manamorphose_ir", &ir);
    insta::assert_json_snapshot!("manamorphose_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// ETB search (tutor)
// ---------------------------------------------------------------------------

#[test]
fn stoneforge_mystic() {
    let (ir, lowered) = parse_two_layer(
        "When this creature enters, you may search your library for an Equipment card, reveal it, put it into your hand, then shuffle.\n{1}{W}, {T}: You may put an Equipment card from your hand onto the battlefield.",
        "Stoneforge Mystic",
        &["Creature"],
        &["Kor", "Artificer"],
    );
    insta::assert_json_snapshot!("stoneforge_mystic_ir", &ir);
    insta::assert_json_snapshot!("stoneforge_mystic_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Figure of Destiny (multi-activated, type-changing)
// ---------------------------------------------------------------------------

#[test]
fn figure_of_destiny() {
    let (ir, lowered) = parse_two_layer(
        "{R/W}: This creature becomes a Kithkin Spirit with base power and toughness 2/2.\n{R/W}{R/W}{R/W}: If this creature is a Spirit, it becomes a Kithkin Spirit Warrior with base power and toughness 4/4.\n{R/W}{R/W}{R/W}{R/W}{R/W}{R/W}: If this creature is a Warrior, it becomes a Kithkin Spirit Warrior Avatar with base power and toughness 8/8, flying, and first strike.",
        "Figure of Destiny",
        &["Creature"],
        &["Kithkin"],
    );
    insta::assert_json_snapshot!("figure_of_destiny_ir", &ir);
    insta::assert_json_snapshot!("figure_of_destiny_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Dies trigger
// ---------------------------------------------------------------------------

#[test]
fn murderous_rider() {
    let (ir, lowered) = parse_two_layer(
        "Lifelink\nWhen this creature dies, put it on the bottom of its owner's library.",
        "Murderous Rider",
        &["Creature"],
        &["Zombie", "Knight"],
    );
    insta::assert_json_snapshot!("murderous_rider_ir", &ir);
    insta::assert_json_snapshot!("murderous_rider_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Soulbond
// ---------------------------------------------------------------------------

#[test]
fn wolfir_silverheart() {
    let (ir, lowered) = parse_two_layer(
        "Soulbond (You may pair this creature with another unpaired creature when either enters. They remain paired for as long as you control both of them.)\nAs long as this creature is paired with another creature, each of those creatures gets +4/+4.",
        "Wolfir Silverheart",
        &["Creature"],
        &["Wolf", "Warrior"],
    );
    insta::assert_json_snapshot!("wolfir_silverheart_ir", &ir);
    insta::assert_json_snapshot!("wolfir_silverheart_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Adventure companion
// ---------------------------------------------------------------------------

#[test]
fn edgewall_innkeeper() {
    let (ir, lowered) = parse_two_layer(
        "Whenever you cast a creature spell that has an Adventure, draw a card. (It doesn't need to have gone on the adventure first.)",
        "Edgewall Innkeeper",
        &["Creature"],
        &["Human", "Peasant"],
    );
    insta::assert_json_snapshot!("edgewall_innkeeper_ir", &ir);
    insta::assert_json_snapshot!("edgewall_innkeeper_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Bomat Courier (exile + activated with complex costs)
// ---------------------------------------------------------------------------

#[test]
fn bomat_courier() {
    let (ir, lowered) = parse_two_layer(
        "Haste\nWhenever this creature attacks, exile the top card of your library face down. (You can't look at it.)\n{R}, Discard your hand, Sacrifice this creature: Put all cards exiled with this creature into their owners' hands.",
        "Bomat Courier",
        &["Artifact", "Creature"],
        &["Construct"],
    );
    insta::assert_json_snapshot!("bomat_courier_ir", &ir);
    insta::assert_json_snapshot!("bomat_courier_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Parity-oracle coverage for otherwise-unsnapshotted document item variants
// (Plan 01, assertion 6).
//
// `CastingRestriction`, `SolveCondition`, and `StriveCost` are producible
// `OracleItemIr` variants that no lowered snapshot in this crate populated:
// across every `*_lowered.snap` here and every `ParsedAbilities` snapshot in
// `parser/snapshots/`, `casting_restrictions` was always empty and
// `solve_condition`/`strive_cost` were always null. The source-order builder
// and the assembly traversal both rewrite the item -> `ParsedAbilities` fold,
// so without these three the fold could drop any of them silently.
// ---------------------------------------------------------------------------

#[test]
fn champions_victory() {
    let (ir, lowered) = parse_two_layer(
        "Cast this spell only during the declare attackers step and only if you've been attacked this step.\nReturn target attacking creature to its owner's hand.",
        "Champion's Victory",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("champions_victory_ir", &ir);
    insta::assert_json_snapshot!("champions_victory_lowered", &lowered);
}

#[test]
fn case_of_the_crimson_pulse() {
    let (ir, lowered) = parse_two_layer(
        "When this Case enters, discard a card, then draw two cards.\nTo solve — You have no cards in hand. (If unsolved, solve at the beginning of your end step.)\nSolved — At the beginning of your upkeep, discard your hand, then draw two cards.",
        "Case of the Crimson Pulse",
        &["Enchantment"],
        &["Case"],
    );
    insta::assert_json_snapshot!("case_of_the_crimson_pulse_ir", &ir);
    insta::assert_json_snapshot!("case_of_the_crimson_pulse_lowered", &lowered);
}

/// CR 719.3c: the **activated** `"Solved — {cost}: {effect}"` shape, which the
/// sibling `case_of_the_crimson_pulse` fixture above does NOT reach — its Solved
/// clause is a triggered ability, so it never passes `find_activated_colon`.
///
/// Landed with T8-A1 because the §5.3 non-vacuity probe measured the gap rather
/// than assuming it: with a `panic!` at the recognizer, **zero** of the 17844
/// `--lib` tests fired, and the only two tests that reach it at all
/// (`case_solve_condition`) assert on `is_solved` and use `"Solved — {T}: Add
/// {R}."` — a fixture with **empty** parsed constraints, so it cannot observe the
/// activation-restriction vector at all. Dropping the implicit
/// `ActivationRestriction::IsSolved` stayed green across every one of them.
///
/// Case of the Stashed Skeleton is chosen because its trailing "Activate only as
/// a sorcery." makes `strip_activated_constraints` yield a **non-empty**
/// `constraints.restrictions`. The snapshot therefore pins both halves of the
/// vector *and their order* — implicit `IsSolved` first (CR 719.3c), parsed
/// `AsSorcery` second (CR 602.5d) — which is the one property of this recognizer
/// that T8's shell conversion could silently normalize away, since the Power-up
/// recognizer composes the same vector in the opposite order.
#[test]
fn case_of_the_stashed_skeleton() {
    let (ir, lowered) = parse_two_layer(
        "When this Case enters, create a 2/1 black Skeleton creature token and suspect it. (It has menace and can't block.)\nTo solve — You control no suspected Skeletons. (If unsolved, solve at the beginning of your end step.)\nSolved — {1}{B}, Sacrifice this Case: Search your library for a card, put it into your hand, then shuffle. Activate only as a sorcery.",
        "Case of the Stashed Skeleton",
        &["Enchantment"],
        &["Case"],
    );
    insta::assert_json_snapshot!("case_of_the_stashed_skeleton_ir", &ir);
    insta::assert_json_snapshot!("case_of_the_stashed_skeleton_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// T8-A2 §5.3 remediation: the activation-restriction ORDER at the four
// keyword-labelled activated recognizers (Channel, Boast, Exhaust, Forecast).
//
// The non-vacuity probe measured these sites as REACHED but their restriction
// *order* as UNWITNESSED. With the conversion in place, swapping the parsed
// constraints against the implicit ones — and swapping the two implicit ones
// against each other — left all 34 reaching tests green, because every existing
// fixture is degenerate on this axis: the reminder text that states the
// restrictions is stripped before `strip_activated_constraints` runs, so
// `constraints.restrictions` is empty and the existing assertions use
// order-insensitive `.contains(..)`.
//
// The four fixtures below are real pool cards (verbatim Oracle text from
// `data/card-data.json`) chosen so each pins an order the conversion could
// otherwise normalize away. Each was watched go RED under the corresponding
// perturbation before being committed.
// ---------------------------------------------------------------------------

/// CR 207.2c Channel, with a NON-degenerate parsed constraint.
///
/// Channel is the one site of the four whose restriction vector is the parsed
/// constraints *alone* — it pushes no implicit restriction — and whose original
/// wrote `=` under an is-empty guard rather than `extend`. The existing Channel
/// fixtures (`boseiju_who_endures` and the two `channel_*` parser tests) all
/// have an EMPTY `constraints.restrictions`, so none of them can observe that
/// vector reaching the lowered definition at all.
///
/// Ghost-Lit Stalker's trailing "Activate only as a sorcery." (CR 602.5d) makes
/// it non-empty on BOTH its lines, so the snapshot pins the parsed constraint
/// surviving the shell's `extend`. Watched red by deleting the
/// `ir.shell.activation_restrictions` assignment: `AsSorcery` disappears from
/// the Channel ability.
#[test]
fn ghost_lit_stalker() {
    let (ir, lowered) = parse_two_layer(
        "{4}{B}, {T}: Target player discards two cards. Activate only as a sorcery.\nChannel — {5}{B}{B}, Discard this card: Target player discards four cards. Activate only as a sorcery.",
        "Ghost-Lit Stalker",
        &["Creature"],
        &["Spirit"],
    );
    insta::assert_json_snapshot!("ghost_lit_stalker_ir", &ir);
    insta::assert_json_snapshot!("ghost_lit_stalker_lowered", &lowered);
}

/// CR 702.177a Exhaust, with a NON-degenerate parsed constraint.
///
/// The only Exhaust card in the pool whose "Activate only as a sorcery."
/// (CR 602.5d) sits OUTSIDE the reminder parentheses, so it is the only one that
/// makes `constraints.restrictions` non-empty. That is what lets this snapshot
/// pin the site's parsed-then-implicit order: parsed `AsSorcery` first, implicit
/// `OnlyOnce` (CR 702.177a) second.
///
/// Watched red by composing the vector implicit-first instead: `OnlyOnce`
/// relocates ahead of `AsSorcery`. `exhaust_mana_cost_parses_as_activated_with_once_per_game_restriction`
/// and the `exhaust_keyword_once_per_permanent` integration tests all stay green
/// under that swap, which is why this fixture is needed.
#[test]
fn liliana_the_repentant() {
    let (ir, lowered) = parse_two_layer(
        "Whenever another creature or planeswalker you control enters, mill two cards.\nExhaust — {5}{B}: Return target creature or planeswalker card from your graveyard to the battlefield. Put a +1/+1 counter on Liliana. Activate only as a sorcery. (Activate each exhaust ability only once.)",
        "Liliana the Repentant",
        &["Creature"],
        &["Human", "Warlock"],
    );
    insta::assert_json_snapshot!("liliana_the_repentant_ir", &ir);
    insta::assert_json_snapshot!("liliana_the_repentant_lowered", &lowered);
}

/// CR 508.1b-c + CR 508.1h + CR 602.2: Onakke's two printed lines exercise both the
/// planeswalker-only combat-tax static and its graveyard activation. Snapshot
/// both document IR and lowering so neither line can silently degrade while
/// the other stays supported.
#[test]
fn onakke_oathkeeper() {
    let (ir, lowered) = parse_two_layer(
        "Creatures can't attack planeswalkers you control unless their controller pays {1} for each creature they control that's attacking a planeswalker you control.\n{4}{W}{W}, Exile this card from your graveyard: Return target planeswalker card from your graveyard to the battlefield.",
        "Onakke Oathkeeper",
        &["Creature"],
        &["Ogre", "Spirit"],
    );
    insta::assert_json_snapshot!("onakke_oathkeeper_ir", &ir);
    insta::assert_json_snapshot!("onakke_oathkeeper_lowered", &lowered);
}

/// CR 702.142a Boast: pins the order of the two IMPLICIT restrictions.
///
/// No Boast card in the pool states its activation instruction outside reminder
/// text, so the parsed-vs-implicit axis is unwitnessable here by any real card
/// (reported as a finding rather than papered over with an invented card). What
/// IS witnessable, and was previously unwitnessed, is the order of the two
/// implicit restrictions relative to each other: this site pushes
/// `OnlyOnceEachTurn` before `RequiresCondition{SourceAttackedThisTurn}`, which
/// is the REVERSE of the order CR 702.142a states them in ("Activate only if
/// this creature attacked this turn and only once each turn"). That inversion is
/// pre-existing, is preserved by the conversion, and is now pinned so a later
/// tranche cannot silently "tidy" it.
///
/// Watched red by swapping the two pushes.
#[test]
fn arni_brokenbrow() {
    let (ir, lowered) = parse_two_layer(
        "Haste\nBoast — {1}: You may change Arni's base power to 1 plus the greatest power among other creatures you control until end of turn. (Activate only if this creature attacked this turn and only once each turn.)",
        "Arni Brokenbrow",
        &["Creature"],
        &["Human", "Berserker"],
    );
    insta::assert_json_snapshot!("arni_brokenbrow_ir", &ir);
    insta::assert_json_snapshot!("arni_brokenbrow_lowered", &lowered);
}

/// CR 702.57a-b Forecast: pins the order of the two IMPLICIT restrictions.
///
/// As with Boast, no Forecast card states its activation instruction outside
/// reminder text, so only the implicit-vs-implicit axis is witnessable. This
/// site pushes `DuringYourUpkeep` before `OnlyOnceEachTurn`, matching the order
/// CR 702.57b states them in. Before this fixture the two `forecast_*` parser
/// tests asserted both restrictions with order-insensitive `.contains(..)`, so a
/// swap was silent.
///
/// Also the only two-layer snapshot coverage Forecast has had; its two parser
/// tests were the entire reaching set, and neither reaches the integration
/// binary.
///
/// Watched red by swapping the two pushes.
#[test]
fn govern_the_guildless() {
    let (ir, lowered) = parse_two_layer(
        "Gain control of target monocolored creature.\nForecast — {1}{U}, Reveal this card from your hand: Target creature becomes the color or colors of your choice until end of turn. (Activate only during your upkeep and only once each turn.)",
        "Govern the Guildless",
        &["Sorcery"],
        &[],
    );
    insta::assert_json_snapshot!("govern_the_guildless_ir", &ir);
    insta::assert_json_snapshot!("govern_the_guildless_lowered", &lowered);
}

#[test]
fn aerial_formation() {
    let (ir, lowered) = parse_two_layer(
        "Strive — This spell costs {2}{U} more to cast for each target beyond the first.\nAny number of target creatures each get +1/+1 and gain flying until end of turn.",
        "Aerial Formation",
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!("aerial_formation_ir", &ir);
    insta::assert_json_snapshot!("aerial_formation_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Class level sections (Plan 05b, T1 witness corpus)
//
// No Class card was in the two-layer corpus, which made T1's byte-identity gate
// vacuous: the `oracle_class.rs` level-section arms could be converted from
// `PreLowered*` to IR nodes with zero snapshot churn and zero proof the conversion
// was reached. All five cards below are pool-verified (Oracle text and `card_type`
// read from `data/card-data.json`, never written from memory).
//
// The arm each line actually reaches was read off the generated baseline
// (node variant + source fragment per item), not predicted — an earlier version of
// this comment guessed three of them wrong. Arm attribution follows from the
// dispatch order in `parse_class_sections` and the predicates in
// `oracle_classifier.rs`; `is_granted_static_line` is checked first and requires a
// prefix from `GRANTED_STATIC_PREFIXES` *and* a verb from `GRANTED_STATIC_VERBS`
// (`has "` / `have "` / `gains "` / `gain "` — the quote is part of the match), so
// only a granted *quoted ability* reaches it. An unquoted grant falls through to
// `is_static_pattern`.
//
//   arm                          unwrapped (level 1)   wrapped (level > 1)
//   ---------------------------  --------------------  ----------------------------
//   granted quoted static (193)  (none)                Sorcerer Class L2
//   plain static (204)           Wizard Class L1       Barbarian L3, Innkeeper L2,
//                                                      Bard L2
//   replacement (221)            Bard Class L1         Innkeeper's Talent L3
//   ability-word static (260)    (none)                (none)
//
// Two coverage gaps are recorded rather than papered over:
//
//   * Row 260 has NO witness in the pool. All three ability-word-prefixed Class
//     level bodies (Druid Class, A-Druid Class, Advanced Floral Invocations) are
//     `Landfall — Whenever ...` and take the trigger arm instead. Its conversion
//     rests on the class argument alone, not on corpus evidence.
//   * Row 193 is witnessed only in its wrapped form. The wrap is applied to
//     `static_def` *before* the push in both branches, so the conversion site is
//     identical either way; the unwrapped half is covered by row 204's Wizard L1.
//
// Non-witness worth knowing about: Barbarian Class L1 ("If you would roll one or
// more dice, instead roll that many dice plus one and ignore the lowest roll")
// does NOT reach the replacement arm — it falls through to the generic path and
// lands as `PreLoweredSpell` with an `Unimplemented` effect. That is a pre-existing
// parser gap, not something T1 introduces; it is baselined here so that if T1
// changes it, the churn is visible and must be explained.
// ---------------------------------------------------------------------------

#[test]
fn sorcerer_class() {
    let (ir, lowered) = parse_two_layer(
        "(Gain the next level as a sorcery to add its ability.)\nWhen this Class enters, draw two cards, then discard two cards.\n{U}{R}: Level 2\nCreatures you control have \"{T}: Add {U} or {R}. Spend this mana only to cast an instant or sorcery spell or to gain a Class level.\"\n{3}{U}{R}: Level 3\nWhenever you cast an instant or sorcery spell, that spell deals damage to each opponent equal to the number of instant and sorcery spells you've cast this turn.",
        "Sorcerer Class",
        &["Enchantment"],
        &["Class"],
    );
    insta::assert_json_snapshot!("sorcerer_class_ir", &ir);
    insta::assert_json_snapshot!("sorcerer_class_lowered", &lowered);
}

#[test]
fn barbarian_class() {
    let (ir, lowered) = parse_two_layer(
        "(Gain the next level as a sorcery to add its ability.)\nIf you would roll one or more dice, instead roll that many dice plus one and ignore the lowest roll.\n{1}{R}: Level 2\nWhenever you roll one or more dice, target creature you control gets +2/+0 and gains menace until end of turn.\n{2}{R}: Level 3\nCreatures you control have haste.",
        "Barbarian Class",
        &["Enchantment"],
        &["Class"],
    );
    insta::assert_json_snapshot!("barbarian_class_ir", &ir);
    insta::assert_json_snapshot!("barbarian_class_lowered", &lowered);
}

#[test]
fn innkeepers_talent() {
    let (ir, lowered) = parse_two_layer(
        "(Gain the next level as a sorcery to add its ability.)\nAt the beginning of combat on your turn, put a +1/+1 counter on target creature you control.\n{G}: Level 2\nPermanents you control with counters on them have ward {1}.\n{3}{G}: Level 3\nIf you would put one or more counters on a permanent or player, put twice that many of each of those kinds of counters on that permanent or player instead.",
        "Innkeeper's Talent",
        &["Enchantment"],
        &["Class"],
    );
    insta::assert_json_snapshot!("innkeepers_talent_ir", &ir);
    insta::assert_json_snapshot!("innkeepers_talent_lowered", &lowered);
}

#[test]
fn bard_class() {
    let (ir, lowered) = parse_two_layer(
        "(Gain the next level as a sorcery to add its ability.)\nLegendary creatures you control enter with an additional +1/+1 counter on them.\n{R}{G}: Level 2\nLegendary spells you cast cost {R}{G} less to cast. This effect reduces only the amount of colored mana you pay.\n{3}{R}{G}: Level 3\nWhenever you cast a legendary spell, exile the top two cards of your library. You may play them this turn.",
        "Bard Class",
        &["Enchantment"],
        &["Class"],
    );
    insta::assert_json_snapshot!("bard_class_ir", &ir);
    insta::assert_json_snapshot!("bard_class_lowered", &lowered);
}

/// Level-1 plain static — the unwrapped half of the `is_static_pattern` arm.
/// `"You have no maximum hand size."` matches `GRANTED_STATIC_PREFIXES` on `"you "`
/// but carries no quoted ability, so it falls past `is_granted_static_line`.
#[test]
fn wizard_class() {
    let (ir, lowered) = parse_two_layer(
        "(Gain the next level as a sorcery to add its ability.)\nYou have no maximum hand size.\n{2}{U}: Level 2\nWhen this Class becomes level 2, draw two cards.\n{4}{U}: Level 3\nWhenever you draw a card, put a +1/+1 counter on target creature you control.",
        "Wizard Class",
        &["Enchantment"],
        &["Class"],
    );
    insta::assert_json_snapshot!("wizard_class_two_layer_ir", &ir);
    insta::assert_json_snapshot!("wizard_class_two_layer_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Preprocessor-assembled triggers (Plan 05b, T5b witnesses)
// ---------------------------------------------------------------------------

/// CR 714 Saga chapter triggers, including the CR 714.2c multi-numeral line.
///
/// The Saga preprocessor hand-builds one `TriggerDefinition` per numeral and
/// stamps `description = "Chapter {n}"` — deliberately NOT the printed line, so
/// this fixture is also the standing regression witness for that stamp. No
/// other card in the two-layer corpus is a Saga, so without it T5b's Saga
/// conversion is snapshot-invisible.
///
/// `I, II — …` shares one source line between two chapters; the emitted pair
/// must keep ascending ordinals on that shared line key (CR 714.2c).
#[test]
fn history_of_benalia() {
    let (ir, lowered) = parse_two_layer(
        "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI, II — Create a 2/2 white Knight creature token with vigilance.\nIII — Knights you control get +2/+1 until end of turn.",
        "History of Benalia",
        &["Enchantment"],
        &["Saga"],
    );
    insta::assert_json_snapshot!("history_of_benalia_ir", &ir);
    insta::assert_json_snapshot!("history_of_benalia_lowered", &lowered);
}

/// CR 717 Attraction visit trigger.
///
/// The Attraction preprocessor hand-builds a `VisitAttraction` trigger and
/// leaves `description` at `None` — the opposite of the Saga stamp, and the
/// reason both belong in the corpus: a lowering path that overwrote
/// `description` from the source line would corrupt Saga's value and invent one
/// for Attraction, and only one fixture would catch each.
///
/// Bumper Cars is the plain `Visit — …` header form. The numbered form
/// (`"1, 3 — …"`, which stamps `AttractionVisitRoll { min, max }`) has **no
/// witness here because it has none in the pool**: zero Attractions in
/// `data/card-data.json` print a numbered visit line.
#[test]
fn bumper_cars() {
    let (ir, lowered) = parse_two_layer(
        "Visit — Target creature must be blocked this turn if able.",
        "Bumper Cars",
        &["Artifact"],
        &["Attraction"],
    );
    insta::assert_json_snapshot!("bumper_cars_ir", &ir);
    insta::assert_json_snapshot!("bumper_cars_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// CR 701.43d exert-as-attacks, all three printed forms (Plan 05b, T5c witnesses)
// ---------------------------------------------------------------------------
//
// Each of the three dispatch arms hand-builds an `Exerted` trigger whose
// `description` is the WHOLE printed line while its `execute` is parsed from
// the text SUFFIX after ". When you do, ". Nothing in the two-layer corpus
// reached any of them before these fixtures, so all three T5c exert
// conversions would have been snapshot-invisible.

/// Bare-`~` form: `"You may exert this creature as it attacks."`
#[test]
fn ahn_crop_champion() {
    let (ir, lowered) = parse_two_layer(
        "You may exert this creature as it attacks. When you do, untap all other creatures you control. (An exerted creature won't untap during your next untap step.)",
        "Ahn-Crop Champion",
        &["Creature"],
        &["Human", "Warrior"],
    );
    insta::assert_json_snapshot!("ahn_crop_champion_ir", &ir);
    insta::assert_json_snapshot!("ahn_crop_champion_lowered", &lowered);
}

/// Card-name form with a gendered pronoun: `"You may exert Themberchaud as he
/// attacks."` — the arm the bare-`~` tags above cannot match, because
/// self-reference normalization rewrites the name but not `"as he attacks"`.
///
/// Themberchaud also prints an ordinary ETB trigger ABOVE the exert line, so
/// this fixture additionally witnesses that converting the exert emission does
/// not disturb the CR 707.9a printed-trigger slot of a preceding trigger.
#[test]
fn themberchaud() {
    let (ir, lowered) = parse_two_layer_with_keywords(
        "Trample\nWhen Themberchaud enters, he deals X damage to each other creature without flying and each player, where X is the number of Mountains you control.\nYou may exert Themberchaud as he attacks. When you do, he gains flying until end of turn. (An exerted creature won't untap during your next untap step.)",
        "Themberchaud",
        &["trample"],
        &["Creature"],
        &["Dragon"],
    );
    insta::assert_json_snapshot!("themberchaud_ir", &ir);
    insta::assert_json_snapshot!("themberchaud_lowered", &lowered);
}

/// Conditional form: `"If this creature hasn't been exerted this turn, …"`.
/// Combat Celebrant is the ONLY card in the pool that reaches this arm.
///
/// Baselines a known pre-existing gap rather than hiding it: the leading
/// if-gate is parsed for dispatch and then **dropped** — the emitted trigger
/// carries no condition for it. That is recorded as a census harvest item; this
/// fixture pins the current (wrong) shape so the conversion is provably
/// behavior-preserving and the gap stays visible for a separate fix.
#[test]
fn combat_celebrant() {
    let (ir, lowered) = parse_two_layer(
        "If this creature hasn't been exerted this turn, you may exert it as it attacks. When you do, untap all other creatures you control and after this phase, there is an additional combat phase. (An exerted creature won't untap during your next untap step.)",
        "Combat Celebrant",
        &["Creature"],
        &["Human", "Warrior"],
    );
    insta::assert_json_snapshot!("combat_celebrant_ir", &ir);
    insta::assert_json_snapshot!("combat_celebrant_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Synthesized flash-cleanup-sacrifice trigger (Plan 05b, T5c witness)
// ---------------------------------------------------------------------------

/// The one recognizer in this tranche whose `execute` is **fully synthesized**:
/// no part of the printed line is parsed into it. The line grants a casting
/// option, and the paired trigger's body — `CreateDelayedTrigger { AtNextPhase
/// (Cleanup) } → Sacrifice { SelfRef }` — is hand-assembled from three
/// `tag()`s, so its shape is a pure function of the recognizer matching at all.
///
/// Armor of Thorns is the alphabetically-first of the pool cards printing this
/// exact sentence; the other arms of the same class (Grave Servitude, Lightning
/// Reflexes, Mystic Veil, …) differ only in the Aura body below it.
#[test]
fn armor_of_thorns() {
    let (ir, lowered) = parse_two_layer(
        "You may cast this spell as though it had flash. If you cast it any time a sorcery couldn't have been cast, the controller of the permanent it becomes sacrifices it at the beginning of the next cleanup step.\nEnchant nonblack creature\nEnchanted creature gets +2/+2.",
        "Armor of Thorns",
        &["Enchantment"],
        &["Aura"],
    );
    insta::assert_json_snapshot!("armor_of_thorns_ir", &ir);
    insta::assert_json_snapshot!("armor_of_thorns_lowered", &lowered);
}

// ---------------------------------------------------------------------------
// Diagnostic snapshot tests (Phase 51, D-10)
// ---------------------------------------------------------------------------

mod diagnostic_snapshots {
    use crate::parser::oracle::parse_oracle_ir;

    /// Parse Oracle text and return only the diagnostics vec from the IR.
    fn parse_diagnostics(
        oracle_text: &str,
        card_name: &str,
        types: &[&str],
        subtypes: &[&str],
    ) -> Vec<crate::parser::oracle_ir::diagnostic::OracleDiagnostic> {
        let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
        let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
        let ir = parse_oracle_ir(oracle_text, card_name, &[], &types, &subtypes);
        ir.diagnostics
    }

    #[test]
    /// CR 117.1 + CR 400.7j + CR 608.2k: Regression guard for Surtland Flinger.
    /// The "If the sacrificed creature was a Giant, ~ deals twice that much
    /// damage instead" override now parses cleanly via
    /// `parse_cost_paid_object_definite_noun_form` (definite-noun form
    /// generalized over noun + type-or-subtype predicate). The instead branch
    /// is captured as a `ConditionInstead { CostPaidObjectMatchesFilter }`,
    /// the trailing "instead" sentinel is consumed by the instead-clause
    /// stripper, and no `TargetFallback` leaks to diagnostics.
    fn diagnostic_target_fallback() {
        let diagnostics = parse_diagnostics(
            "Whenever this creature attacks, you may sacrifice another creature. When you do, this creature deals damage equal to the sacrificed creature's power to any target. If the sacrificed creature was a Giant, this creature deals twice that much damage instead.",
            "Surtland Flinger",
            &["Creature"],
            &["Giant", "Berserker"],
        );
        insta::assert_json_snapshot!("diagnostic_target_fallback", &diagnostics);
    }

    #[test]
    fn diagnostic_ignored_remainder() {
        let diagnostics = parse_diagnostics(
            "Whenever this creature attacks, it deals damage to the player or planeswalker it's attacking equal to the number of artifacts you control.\nEncore {5}{R} ({5}{R}, Exile this card from your graveyard: For each opponent, create a token copy that attacks that opponent this turn if able. They gain haste. Sacrifice them at the beginning of the next end step. Activate only as a sorcery.)",
            "Fathom Fleet Swordjack",
            &["Creature"],
            &["Orc", "Pirate"],
        );
        assert!(
            diagnostics
                .iter()
                .any(|d| d.category_name() == "ignored-remainder"),
            "Expected ignored-remainder diagnostic for Fathom Fleet Swordjack, got: {:?}",
            diagnostics
        );
        insta::assert_json_snapshot!("diagnostic_ignored_remainder", &diagnostics);
    }

    #[test]
    fn diagnostic_swallowed_clause_cleared_for_a_killer() {
        // Regression guard for S07 N2: A Killer Among Us' ETB "Then secretly
        // choose Human, Merfolk, or Goblin" used to be a swallowed clause (the
        // enumerated creature-type choice was unrecognized). The new
        // `parse_creature_type_enumeration` arm in `try_parse_named_choice` now
        // parses it as `ChoiceType::CreatureType { options }`, so no
        // swallowed-clause diagnostic is emitted.
        //
        // The ETB now creates all THREE tokens: the comma-listed same-verb token
        // chain ("create A, a B, and a C token") N-way split fix (commit
        // f2648a0cb) no longer drops the MIDDLE element (Merfolk). Full cast-path
        // coverage lives in `crates/engine/tests/a_killer_among_us.rs`.
        let diagnostics = parse_diagnostics(
            "When this enchantment enters, create a 1/1 white Human creature token, a 1/1 blue Merfolk creature token, and a 1/1 red Goblin creature token. Then secretly choose Human, Merfolk, or Goblin.\nSacrifice this enchantment, Reveal the creature type you chose: If target attacking creature token is the chosen type, put three +1/+1 counters on it and it gains deathtouch until end of turn.",
            "A Killer Among Us",
            &["Enchantment"],
            &[],
        );
        assert!(
            !diagnostics
                .iter()
                .any(|d| d.category_name() == "swallowed-clause"),
            "Expected NO swallowed-clause diagnostic for A Killer Among Us after N2, got: {:?}",
            diagnostics
        );
    }

    // NOTE: CascadeLoss diagnostic is not triggered by any card in the current
    // card-data.json corpus (0 occurrences in coverage report). The variant exists
    // for cascade-diff detection in swallow_check.rs but no current Oracle text
    // triggers it. A test will be added when a card that produces this diagnostic
    // is identified.
}

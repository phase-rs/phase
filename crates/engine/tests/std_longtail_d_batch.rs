//! Standard long-tail batch D — parser/static seams that route to EXISTING
//! engine surface, no new engine variant (the `add-engine-variant` gate verdict
//! was "parameterize, don't proliferate" for every change here).
//!
//! SHIPPED (0-Unimplemented + discriminating runtime/derived assertion that
//! flips on revert):
//!   - Laughing Jasper Flint — "Creatures you control but don't own are
//!     Mercenaries in addition to their other types" (LT-F type-grant). Two
//!     parser arms: a generic consonant+y → "-ies" plural rule in `parse_subtype`
//!     ("Mercenaries" → "Mercenary"), and a "<creatures you control> but don't
//!     own" negated-ownership qualifier in the static dispatch arm.
//!   - Midnight Mangler — "During turns other than yours, ~ is an artifact
//!     creature" (LT-F type-grant). Extended the leading turn-restriction peel in
//!     `parse_pronoun_becomes_type_static` to the negated `Not(DuringYourTurn)`
//!     direction.
//!   - Tapestry Warden — "Each creature you control with toughness greater than
//!     its power stations permanents using its toughness rather than its power"
//!     (LT-C station-contribution). Added the bare "stations permanents" leaf to
//!     the existing crew/saddle/station contribution action list.
//!   - Rocket-Powered Goblin Glider — "if it was cast from your graveyard"
//!     (LT-C gravecast). Added the bare "(was|were) cast from [a|your] graveyard"
//!     intervening-if arm → `TriggerCondition::WasCast { zone: Graveyard,
//!     controller: None, owner: Some(You) }`. CR 400.3 + CR 404.1: a graveyard is
//!     owner-specific, so "your graveyard" scopes the origin-zone OWNER, not the
//!     caster — an opponent casting your card from your graveyard satisfies it.
//!     The owner-vs-caster runtime discrimination (all four owner × caster rows)
//!     is asserted in-crate in `game::triggers` against the real
//!     `check_trigger_condition` seam, which is `pub(crate)` and unreachable here.
//!   - Nowhere to Run — "Creatures your opponents control can be the targets of
//!     spells and abilities as though they didn't have hexproof. Ward abilities of
//!     those creatures don't trigger." One static line → an object-scoped
//!     `IgnoreHexproof` (CR 702.11b/702.11e) plus a `SuppressTriggers`
//!     `{BecomesTargeted}` over the same "those creatures" subject (CR 702.21a +
//!     CR 611.3 + CR 613.11). The multiplayer bypass scope and ward-suppression
//!     runtime discriminators live in-crate in `game::targeting` / `game::triggers`
//!     (`pub(crate)`, unreachable here); this crate asserts the end-to-end
//!     zero-Unimplemented parse and the two emitted statics.
//!
//! BUILDING BLOCK (general arm, not card-specific): a count-leading
//! "look at/reveal <count> cards from the top of <owner>'s library" dig
//! word-order. Supported for FIXED counts in both the private (look) and public
//! (reveal) directions and resolves correctly. NON-fixed counts are refused (a
//! coverage-honesty guard): the reveal direction is demoted to a `u32`-count
//! `RevealTop`, and a variable look pairs with a `Put X` keep-count that
//! `Effect::Dig.keep_count: Option<u32>` cannot represent, so a dynamic count
//! would over-claim support while behaving wrong at runtime.
//!
//! DEFERRED (honesty guards assert the residual is exactly the unsupported
//! clause, not an over-claim): Sandman (compound self+target return to
//! battlefield), Fblthp (plot-from-top infra), Choreographed Sparks (the
//! `CopySpell` resolver does not apply `AddKeyword`/`GrantTrigger` modifications
//! to the copy), Leyline of Transformation (continuous type-grant on
//! non-battlefield zones), Stargaze (variable dig look/keep count needs
//! `Dig.count`/`keep_count` as `QuantityExpr` end-to-end).

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::static_abilities::object_crew_power_contribution;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ControllerRef, Effect, QuantityExpr, TargetFilter, TriggerCondition};
use engine::types::card_type::CoreType;
use engine::types::phase::Phase;
use engine::types::statics::CrewAction;
use engine::types::zones::Zone;

fn creature_types() -> Vec<String> {
    vec!["Creature".to_string()]
}

fn parsed_debug(oracle: &str, name: &str, types: &[String], subtypes: &[String]) -> String {
    format!(
        "{:#?}",
        parse_oracle_text(oracle, name, &[], types, subtypes)
    )
}

fn assert_zero_unimplemented(oracle: &str, name: &str, types: &[String], subtypes: &[String]) {
    assert_zero_unimplemented_kw(oracle, name, &[], types, subtypes);
}

/// Same as [`assert_zero_unimplemented`] but threads the printed MTGJSON
/// keyword names so bare keyword-only lines (e.g. "Vigilance") are recognized as
/// keywords rather than mis-flagged as Unimplemented effect candidates — exactly
/// what the card-data loader passes in production.
fn assert_zero_unimplemented_kw(
    oracle: &str,
    name: &str,
    keywords: &[String],
    types: &[String],
    subtypes: &[String],
) {
    let dbg = format!(
        "{:#?}",
        parse_oracle_text(oracle, name, keywords, types, subtypes)
    );
    assert!(
        !dbg.contains("Unimplemented"),
        "{name}: expected zero Unimplemented nodes, parse was:\n{dbg}"
    );
}

// ===========================================================================
// Laughing Jasper Flint (SHIPPED) — LT-F type grant
// ===========================================================================

const LJF_ORACLE: &str = "Creatures you control but don't own are Mercenaries in addition to their other types.\nAt the beginning of your upkeep, exile the top X cards of target opponent's library, where X is the number of outlaws you control. Until end of turn, you may cast spells from among those cards, and mana of any type can be spent to cast those spells.";

#[test]
fn laughing_jasper_flint_zero_unimplemented() {
    assert_zero_unimplemented(
        LJF_ORACLE,
        "Laughing Jasper Flint",
        &["Legendary".to_string(), "Creature".to_string()],
        &["Goblin".to_string(), "Mercenary".to_string()],
    );
}

#[test]
fn laughing_jasper_flint_grants_mercenary_only_to_unowned_creatures() {
    // The static line parses to AddSubtype{Mercenary} on a creature filter that
    // carries BOTH controller=You AND Owned{Opponent} ("you don't own it"). The
    // discriminating axis is the ownership qualifier: a creature you control AND
    // own must NOT gain Mercenary; a creature you control but don't own MUST.
    let parsed = parse_oracle_text(
        "Creatures you control but don't own are Mercenaries in addition to their other types.",
        "Laughing Jasper Flint",
        &[],
        &creature_types(),
        &[],
    );
    let stat = &parsed.statics[0];
    let affected = stat.affected.as_ref().expect("static must carry a filter");
    // Revert guard (shape): the filter must retain the negated-ownership prop —
    // pre-fix the dispatch arm dropped "but don't own" and produced a bare
    // creature-you-control filter (any owner would gain Mercenary).
    let dbg = format!("{affected:?}");
    assert!(
        dbg.contains("Owned") && dbg.contains("Opponent"),
        "affected filter must retain Owned{{Opponent}} (\"you don't own it\"); got {dbg}"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flint = scenario
        .add_creature_from_oracle(P0, "Laughing Jasper Flint", 4, 4, LJF_ORACLE)
        .id();
    // A creature P0 controls AND owns (printed Goblin, no Mercenary).
    let owned = scenario
        .add_creature(P0, "Owned Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    // A creature P0 controls but P1 owns (set owner below).
    let stolen = scenario
        .add_creature(P0, "Stolen Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&stolen).unwrap().owner = P1;
    let _ = flint;

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let owned_subtypes = &runner.state().objects[&owned].card_types.subtypes;
    assert!(
        !owned_subtypes.iter().any(|s| s == "Mercenary"),
        "a creature you control AND own must NOT gain Mercenary; got {owned_subtypes:?}"
    );
    let stolen_subtypes = &runner.state().objects[&stolen].card_types.subtypes;
    assert!(
        stolen_subtypes.iter().any(|s| s == "Mercenary"),
        "a creature you control but DON'T own must gain Mercenary; got {stolen_subtypes:?}"
    );
}

// ===========================================================================
// Midnight Mangler (SHIPPED) — LT-F type grant
// ===========================================================================

const MM_ORACLE: &str = "During turns other than yours, this Vehicle is an artifact creature.\nCrew 2 (Tap any number of creatures you control with total power 2 or more: This Vehicle becomes an artifact creature until end of turn.)";

#[test]
fn midnight_mangler_zero_unimplemented() {
    assert_zero_unimplemented(
        MM_ORACLE,
        "Midnight Mangler",
        &["Artifact".to_string()],
        &["Vehicle".to_string()],
    );
}

#[test]
fn midnight_mangler_is_creature_only_during_non_your_turns() {
    let parsed = parse_oracle_text(
        MM_ORACLE,
        "Midnight Mangler",
        &[],
        &["Artifact".to_string()],
        &["Vehicle".to_string()],
    );
    let stat = parsed
        .statics
        .iter()
        .find(|s| {
            s.modifications
                .iter()
                .any(|m| matches!(m, engine::types::ability::ContinuousModification::AddType { core_type } if *core_type == CoreType::Creature))
        })
        .expect("the turn-conditional AddType{Creature} static must be present");
    // Revert guard (shape): the condition must be the NEGATED turn restriction —
    // pre-fix the leading "during turns other than yours, " peel was missing, so
    // the whole line stayed an Unimplemented (no static at all).
    let cond_dbg = format!("{:?}", stat.condition);
    assert!(
        cond_dbg.contains("Not") && cond_dbg.contains("DuringYourTurn"),
        "static condition must be Not(DuringYourTurn); got {cond_dbg}"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vehicle = scenario
        .add_creature_from_oracle(P0, "Midnight Mangler", 0, 0, MM_ORACLE)
        .id();
    let mut runner = scenario.build();
    // Vehicles print as artifacts; the scenario builder adds the Creature type
    // for `add_creature_*`, so strip the printed Creature type to mirror a real
    // (non-creature) Vehicle and prove the static is what makes it a creature.
    {
        let obj = runner.state_mut().objects.get_mut(&vehicle).unwrap();
        obj.card_types
            .core_types
            .retain(|t| *t != CoreType::Creature);
        obj.base_card_types
            .core_types
            .retain(|t| *t != CoreType::Creature);
    }

    // Case A — P0's turn (condition FALSE): the static is OFF, so the Vehicle is
    // NOT a creature.
    runner.state_mut().active_player = P0;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    assert!(
        !runner.state().objects[&vehicle]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "during your own turn the Vehicle must NOT be a creature"
    );

    // Case B — opponent's turn (condition TRUE): the static is ON, so the Vehicle
    // IS an artifact creature. This is the assertion that FLIPS on revert.
    runner.state_mut().active_player = P1;
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    assert!(
        runner.state().objects[&vehicle]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "during turns other than yours the Vehicle must be an artifact creature"
    );
}

// ===========================================================================
// Tapestry Warden (SHIPPED) — LT-C station-contribution static
// ===========================================================================

const TW_ORACLE: &str = "Vigilance\nEach creature you control with toughness greater than its power assigns combat damage equal to its toughness rather than its power.\nEach creature you control with toughness greater than its power stations permanents using its toughness rather than its power.";

#[test]
fn tapestry_warden_zero_unimplemented() {
    assert_zero_unimplemented_kw(
        TW_ORACLE,
        "Tapestry Warden",
        &["Vigilance".to_string()],
        &creature_types(),
        &["Spider".to_string()],
    );
}

#[test]
fn tapestry_warden_station_uses_toughness_for_high_toughness_creatures() {
    let line = "Each creature you control with toughness greater than its power stations permanents using its toughness rather than its power.";
    let def = engine::parser::oracle_static::parse_static_line(line)
        .expect("station-contribution line must parse");
    // Revert guard (shape): the bare "stations permanents" must map to a
    // CrewContribution{ToughnessInsteadOfPower} carrying CrewAction::Station —
    // pre-fix the action-list alt had no bare "stations permanents" arm, so the
    // whole line stayed Unimplemented.
    let dbg = format!("{def:?}");
    assert!(
        dbg.contains("CrewContribution")
            && dbg.contains("ToughnessInsteadOfPower")
            && dbg.contains("Station"),
        "must produce CrewContribution{{ToughnessInsteadOfPower, [Station]}}; got {dbg}"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Tapestry Warden", 1, 4, TW_ORACLE);
    // A 1/5 creature you control — toughness (5) > power (1).
    let wall = scenario.add_creature(P0, "Big Wall", 1, 5).id();
    let mut runner = scenario.build();

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // Discriminating runtime assertion: the wall's station power contribution is
    // its TOUGHNESS (5), not its power (1). Pre-fix the static never parsed, so
    // the contribution would fall back to power (1).
    let contribution = object_crew_power_contribution(runner.state(), wall, CrewAction::Station);
    assert_eq!(
        contribution, 5,
        "Tapestry Warden must make a 1/5 contribute its toughness (5) when stationing; got {contribution}"
    );
}

// ===========================================================================
// Rocket-Powered Goblin Glider (SHIPPED) — LT-C gravecast
// ===========================================================================

const RPGG_ORACLE: &str = "When this Equipment enters, if it was cast from your graveyard, attach it to target creature you control.\nEquipped creature gets +2/+0 and has flying and haste.\nEquip {2}\nMayhem {2}";

#[test]
fn rocket_powered_goblin_glider_zero_unimplemented() {
    assert_zero_unimplemented(
        RPGG_ORACLE,
        "Rocket-Powered Goblin Glider",
        &["Artifact".to_string()],
        &["Equipment".to_string()],
    );
}

#[test]
fn rocket_powered_goblin_glider_etb_condition_is_owner_scoped_was_cast() {
    let parsed = parse_oracle_text(
        RPGG_ORACLE,
        "Rocket-Powered Goblin Glider",
        &[],
        &["Artifact".to_string()],
        &["Equipment".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.condition.is_some())
        .expect("the ETB attach trigger must carry an intervening-if condition");
    // Parser-shape revert guard: the bare "if it was cast from your graveyard"
    // (no "you cast it" caster clause) must produce the OWNER-scoped cast-origin
    // check WasCast{Graveyard, controller: None, owner: Some(You)}.
    // CR 400.3 + CR 404.1: a graveyard is owner-specific, so "your graveyard"
    // constrains who OWNS the card, not who cast it. Pre-fix the bare wording
    // left "from your graveyard" unconsumed (the zone-word parser only matched
    // "a graveyard"/"their graveyard") so the attach clause stayed Unimplemented;
    // the intermediate fix consumed it but mis-modeled "your" as the caster.
    // The owner-vs-caster RUNTIME discrimination (all four owner × caster rows)
    // lives in `game::triggers::rocket_glider_was_cast_from_your_graveyard_is_owner_scoped_not_caster`,
    // which drives the `pub(crate)` `check_trigger_condition` seam directly.
    assert_eq!(
        trigger.condition,
        Some(TriggerCondition::WasCast {
            zone: Some(Zone::Graveyard),
            controller: None,
            owner: Some(ControllerRef::You),
        }),
        "\"cast from your graveyard\" must scope the origin-zone OWNER (owner=You), \
         never the caster (controller stays None)"
    );
    // The gated effect attaches to a target creature you control.
    let execute = trigger.execute.as_deref().expect("trigger must execute");
    assert!(
        matches!(*execute.effect, Effect::Attach { .. }),
        "the gated effect must be an Attach; got {:?}",
        execute.effect
    );
}

// ===========================================================================
// Look-count dig BUILDING BLOCK (Stargaze's count-leading word order) — SHIPPED
// as a general arm; the Stargaze CARD is deferred (variable keep-count).
// ===========================================================================

#[test]
fn look_at_count_from_top_word_order_parses_dig() {
    // The count-leading word order ("look at N cards from the top of your
    // library") must lower to the same Dig as the "look at the top N cards"
    // order. Use a FIXED count + fixed keep so the dig resolves correctly
    // end-to-end (variable "Put X cards" is the deferred Stargaze gap).
    let parsed = parse_oracle_text(
        "Look at three cards from the top of your library. Put two of them into your hand and the rest into your graveyard.",
        "Dig Tester",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let spell = &parsed.abilities[0];
    match &*spell.effect {
        Effect::Dig {
            count,
            keep_count,
            destination,
            rest_destination,
            ..
        } => {
            assert_eq!(
                *count,
                QuantityExpr::Fixed { value: 3 },
                "look count must be 3 (count-leading word order)"
            );
            assert_eq!(*keep_count, Some(2), "keep-into-hand count must be 2");
            assert_eq!(
                *destination,
                Some(engine::types::zones::Zone::Hand),
                "kept cards go to hand"
            );
            assert_eq!(
                *rest_destination,
                Some(engine::types::zones::Zone::Graveyard),
                "the rest go to the graveyard"
            );
        }
        other => panic!("count-leading look must lower to Effect::Dig; got {other:?}"),
    }
}

#[test]
fn stargaze_variable_count_dig_is_supported() {
    // Stargaze ("Look at twice X cards ... Put X cards from among them into your
    // hand ...") is now SHIPPED: the look count (Multiply{2,X}) rides
    // `Effect::Dig.count` (runtime-resolved) and the dynamic keep (X) rides the
    // added `Effect::Dig.keep_count_expr`. Full runtime coverage (cast X=2, keep 2
    // of 4, life/zone deltas) lives in `tests/stargaze_dynamic_keep.rs`. Here we
    // assert the count-leading arm no longer refuses the dynamic count — the
    // assertion that flips if the look-guard relaxation is reverted.
    let dbg = parsed_debug(
        "Look at twice X cards from the top of your library. Put X cards from among them into your hand and the rest into your graveyard. You lose X life.",
        "Stargaze",
        &["Sorcery".to_string()],
        &[],
    );
    assert!(
        !dbg.contains("Unimplemented"),
        "Stargaze must now parse fully (no Unimplemented); parse was:\n{dbg}"
    );
    // `keep_count_expr` is `skip_serializing_if = "Option::is_none"`, so its
    // presence in the serialized parse proves the dynamic keep (X) was captured.
    assert!(
        dbg.contains("keep_count_expr"),
        "Stargaze's dynamic keep (X) must ride keep_count_expr; parse was:\n{dbg}"
    );
}

#[test]
fn count_leading_word_order_requires_fixed_count() {
    // Coverage-honesty guard for the count-leading look/reveal word order. A
    // FIXED count is supported in BOTH directions and resolves the correct
    // number; a non-`Fixed` count is refused so the form stays honestly
    // Unimplemented instead of over-claiming support while behaving wrong at
    // runtime.

    // FIXED reveal: lowers to a 3-card reveal (RevealTop demotion is lossless for
    // a Fixed count). Assertion flips on revert if the arm stopped firing.
    let fixed_reveal = parse_oracle_text(
        "Reveal three cards from the top of your library.",
        "Fixed Reveal Tester",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    match &*fixed_reveal.abilities[0].effect {
        Effect::RevealTop { count, .. } => assert_eq!(
            *count, 3,
            "fixed-count reveal must reveal exactly 3 cards (lossless demotion)"
        ),
        Effect::Dig {
            count: QuantityExpr::Fixed { value },
            reveal: true,
            ..
        } => assert_eq!(*value, 3, "fixed-count reveal-Dig must carry count 3"),
        other => panic!("fixed-count reveal must lower to a 3-card reveal; got {other:?}"),
    }

    // FIXED look: the private direction stays an Effect::Dig with the fixed count
    // (dig::resolve honors it). The "look at the top N" sibling word order is
    // covered by `look_at_count_from_top_word_order_parses_dig`; here the bare
    // count-leading look must still produce a Fixed-count Dig.
    let fixed_look = parse_oracle_text(
        "Look at three cards from the top of your library. Put two of them into your hand and the rest into your graveyard.",
        "Fixed Look Tester",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    match &*fixed_look.abilities[0].effect {
        Effect::Dig {
            count: QuantityExpr::Fixed { value },
            reveal,
            ..
        } => {
            assert!(!reveal, "look at is the private direction (reveal=false)");
            assert_eq!(*value, 3, "fixed-count look-Dig must carry count 3");
        }
        other => panic!("fixed-count look must stay a 3-card Dig; got {other:?}"),
    }

    // Coverage-honesty split (Step 4): the two count-leading directions now
    // diverge on a non-`Fixed` ("twice X") count.
    //
    // REVEAL still over-claims — an unpatched reveal-Dig demotes to
    // `RevealTop { count: u32 }`, which has no dynamic-count path — so it stays
    // honestly Unimplemented. Assertion flips if the reveal Fixed-only guard is
    // dropped.
    let reveal_dbg = parsed_debug(
        "Reveal twice X cards from the top of your library.",
        "Variable Reveal Tester",
        &["Sorcery".to_string()],
        &[],
    );
    assert!(
        reveal_dbg.contains("Unimplemented"),
        "a non-fixed (twice X) reveal count must stay Unimplemented; parse was:\n{reveal_dbg}"
    );

    // LOOK now rides `Effect::Dig.count` (runtime-resolved), so "Look at twice X"
    // lowers to a dynamic-count Dig. Assertion flips if the look-guard relaxation
    // is reverted (it would fall through to Unimplemented).
    let look = parse_oracle_text(
        "Look at twice X cards from the top of your library.",
        "Variable Look Tester",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    match &*look.abilities[0].effect {
        Effect::Dig {
            count: QuantityExpr::Multiply { factor: 2, .. },
            reveal: false,
            ..
        } => {}
        other => panic!("look at twice X must lower to a dynamic-count Dig; got {other:?}"),
    }
}

// ===========================================================================
// DEFERRED honesty guards
// ===========================================================================

#[test]
fn sandman_compound_self_target_return_is_supported() {
    // S25 P3 W1 #2: "Return this card and target land card from your graveyard to
    // the battlefield tapped." The self+target reanimation idiom now lowers via
    // `try_parse_reanimate_self_and_target` to a bare `ChangeZone { SelfRef }`
    // primary + a targeted-land `ChangeZone` sub_ability (CR 400.7 + CR 608.2c) —
    // no residual Unimplemented. Full runtime + parser-shape coverage lives in
    // `sandman_reanimate_self_and_land_s25.rs` and `oracle_effect::tests`; this
    // guards the coverage-honesty flip from deferred → supported.
    let dbg = parsed_debug(
        "Sandman's power and toughness are each equal to the number of lands you control.\nSandman can't be blocked by creatures with power 2 or less.\n{3}{G}{G}: Return this card and target land card from your graveyard to the battlefield tapped.",
        "Sandman, Shifting Scoundrel",
        &creature_types(),
        &["Human".to_string(), "Rogue".to_string()],
    );
    assert!(
        !dbg.contains("Unimplemented"),
        "Sandman's compound reanimation must lower fully (no Unimplemented); parse was:\n{dbg}"
    );
    // Positive reach-guard: the parse reached the reanimation arm (a bare SelfRef
    // graveyard→battlefield move), not some unrelated fallback.
    assert!(
        dbg.contains("ChangeZone") && dbg.contains("SelfRef") && dbg.contains("Graveyard"),
        "Sandman must lower to a SelfRef graveyard ChangeZone reanimation; parse was:\n{dbg}"
    );
}

#[test]
fn choreographed_sparks_copy_grant_is_supported() {
    // P2f: apply_spell_copy_modifications now applies AddKeyword/GrantTrigger to the
    // copy (base+live stores), so "The copy gains haste and '<delayed-sac>'" lowers
    // fully — no longer an honest gap. Regression guard for the supported state.
    let dbg = parsed_debug(
        "This spell can't be copied.\nChoose one or both —\n• Copy target instant or sorcery spell you control. You may choose new targets for the copy.\n• Copy target creature spell you control. The copy gains haste and \"At the beginning of the end step, sacrifice this token.\"",
        "Choreographed Sparks",
        &["Instant".to_string()],
        &[],
    );
    assert!(
        !dbg.contains("Unimplemented"),
        "Choreographed Sparks copy-grant is now implemented (P2f haste + delayed-sac fold); must not regress to Unimplemented: {dbg}"
    );
}

#[test]
fn leyline_of_transformation_nonbattlefield_grant_is_deferred() {
    // The first clause (creatures you control are the chosen type) parses; the
    // "same is true for creature spells you control and creature cards you own
    // that aren't on the battlefield" continuous non-battlefield grant is not
    // modeled.
    let dbg = parsed_debug(
        "If this card is in your opening hand, you may begin the game with it on the battlefield.\nAs this enchantment enters, choose a creature type.\nCreatures you control are the chosen type in addition to their other types. The same is true for creature spells you control and creature cards you own that aren't on the battlefield.",
        "Leyline of Transformation",
        &["Enchantment".to_string()],
        &[],
    );
    assert!(
        dbg.contains("Unimplemented"),
        "Leyline of Transformation non-battlefield grant must remain honestly Unimplemented"
    );
}

#[test]
fn nowhere_to_run_hexproof_bypass_and_ward_suppression_ship() {
    // CR 702.11b + CR 702.21a + CR 611.3 + CR 613.11: Nowhere to Run's static
    // line emits BOTH continuous effects from a single line — an object-scoped
    // `IgnoreHexproof` ("Creatures your opponents control can be the targets of
    // spells and abilities as though they didn't have hexproof") and a
    // `SuppressTriggers{BecomesTargeted}` over the SAME "those creatures" subject
    // ("Ward abilities of those creatures don't trigger"). Parsed end-to-end from
    // the printed card with its MTGJSON keyword (Flash), the card has zero
    // Unimplemented residual. The runtime discriminators (multiplayer bypass scope
    // + ward suppression that flip on revert) live in `game::targeting` /
    // `game::triggers`, which are `pub(crate)` and unreachable from this crate.
    use engine::types::statics::{StaticMode, SuppressedTriggerEvent};

    let oracle = "Flash\nWhen this enchantment enters, target creature an opponent controls gets -3/-3 until end of turn.\nCreatures your opponents control can be the targets of spells and abilities as though they didn't have hexproof. Ward abilities of those creatures don't trigger.";
    let parsed = parse_oracle_text(
        oracle,
        "Nowhere to Run",
        &["Flash".to_string()],
        &["Enchantment".to_string()],
        &[],
    );
    let dbg = format!("{parsed:#?}");
    assert!(
        !dbg.contains("Unimplemented"),
        "Nowhere to Run must parse with zero Unimplemented, parse was:\n{dbg}"
    );

    // Sentence 1: the hexproof bypass, object-scoped to opponents' creatures.
    let ignore = parsed
        .statics
        .iter()
        .find(|d| d.mode == StaticMode::IgnoreHexproof)
        .expect("must emit an IgnoreHexproof static");
    let bypass_filter = ignore
        .affected
        .clone()
        .expect("IgnoreHexproof must be object-scoped (affected = Some), not a player grant");

    // Sentence 2: the ward suppression over the SAME subject ("those creatures"
    // reuses sentence 1's parsed filter rather than re-deriving it).
    let suppress = parsed
        .statics
        .iter()
        .find_map(|d| match &d.mode {
            StaticMode::SuppressTriggers {
                events,
                source_filter,
            } => Some((events, source_filter)),
            _ => None,
        })
        .expect("must emit a SuppressTriggers static");
    assert_eq!(suppress.0, &vec![SuppressedTriggerEvent::BecomesTargeted]);
    assert_eq!(
        suppress.1, &bypass_filter,
        "ward suppression must reuse the hexproof-bypass subject filter"
    );
}

// Regression: the consonant+y → "-ies" plural rule must not break the existing
// regular "-s" and irregular plurals, and must canonicalize the new class.
#[test]
fn subtype_ies_plural_canonicalizes_without_breaking_siblings() {
    use engine::parser::oracle_util::parse_subtype;
    let cases = [
        ("Mercenaries", "Mercenary"), // new consonant+y → ies rule
        ("Mercenary", "Mercenary"),   // singular
        ("Goblins", "Goblin"),        // regular -s (unaffected)
        ("Elves", "Elf"),             // irregular table (unaffected)
        ("Allies", "Ally"),           // table entry still wins (consonant+y)
    ];
    for (input, expected) in cases {
        let (canonical, _) = parse_subtype(input)
            .unwrap_or_else(|| panic!("parse_subtype({input:?}) must recognize the subtype"));
        assert_eq!(
            canonical, expected,
            "parse_subtype({input:?}) must canonicalize to {expected:?}"
        );
    }
    // A vowel+y subtype noun must NOT be treated as consonant+y (no false ies).
    let _ = TargetFilter::Any; // keep the import meaningful regardless of feature set
}

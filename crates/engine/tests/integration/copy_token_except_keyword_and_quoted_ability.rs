//! Copy-cluster (std batch) regression coverage for two parser fixes that
//! enrich the `, except <body>` clause of a token-copy effect (CR 707.9a):
//!
//! 1. **Keyword + quoted-ability except body** — `"…except it has haste and
//!    \"At the beginning of the end step, sacrifice this token.\""` (Chandra,
//!    Flameshaper [+1]). Before the fix, `parse_it_has_keywords` consumed the
//!    entire tail as a keyword list and silently dropped the quoted ability, so
//!    the token kept haste but never gained the end-step sacrifice trigger.
//!    `become_copy_except::parse_it_has_keywords_then_quoted_ability` now peels
//!    the quoted ability off the keyword segment at ` and "`.
//!
//! 2. **Comma-anded keyword body inside an except clause** — `"…except it isn't
//!    legendary, is a Reflection in addition to its other types, and has
//!    haste."` (The Apprentice's Folly I/II). Before the fix, the clause
//!    splitter bisected the body at the comma before "and has haste" ("has"
//!    deconjugates to the clause verb "have"), orphaning "has haste" as an
//!    Unimplemented sub_ability so the token never gained haste. The comma
//!    splitter now suppresses the boundary when the chunk-so-far is inside an
//!    except clause and the continuation is a recognised except body.
//!
//! Both tests drive the real cast → stack → resolve pipeline through the
//! scenario runner. Reverting either parser fix flips the named assertion (the
//! copy token would lack the end-step sacrifice, or lack haste).
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 707.2: a token that's a copy of an object copies its copiable values.
//!   - CR 707.9a: a copy effect may cause the copy to gain an ability as part
//!     of the copying process.

use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::scenario::{GameScenario, P0};
use engine::types::card_type::Supertype;
use engine::types::keywords::KeywordKind;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// CR 707.9a + CR 707.2: a token copy whose except clause grants a keyword AND
/// a quoted end-step-sacrifice ability must carry BOTH onto the created token —
/// the token has haste and is sacrificed at the next end step.
#[test]
fn copy_token_except_keyword_then_quoted_sacrifice_grants_both() {
    let mut scenario = GameScenario::new();
    // Cast in the post-combat main phase so advancing to the end step does not
    // stop at the declare-attackers turn-based action (CR 508.1).
    scenario.at_phase(Phase::PostCombatMain);

    // Copy source: a vanilla 3/3 P0 controls.
    let source = scenario.add_creature(P0, "Bear", 3, 3).id();

    // Synthetic sorcery carrying the Chandra-class copy line as a class fix
    // (not a single-card special case).
    let sorcery = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic Flame Copier",
            false,
            "Create a token that's a copy of target creature you control, except it has haste \
             and \"At the beginning of the end step, sacrifice this token.\"",
        )
        .id();

    let mut runner = scenario.build();
    runner.cast(sorcery).target_object(source).resolve();

    // The copy token exists, is a copy of Bear, and has haste.
    let token_id = {
        let state = runner.state();
        let tokens: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|o| o.is_token && o.name == "Bear")
            .collect();
        assert_eq!(
            tokens.len(),
            1,
            "exactly one copy token of Bear must be created"
        );
        let token = tokens[0];
        assert!(
            object_has_effective_keyword_kind(state, token.id, KeywordKind::Haste),
            "CR 707.9a: the copy token must gain haste from the except clause"
        );
        token.id
    };

    // Advance to the end step: the granted "At the beginning of the end step,
    // sacrifice this token" trigger must fire and move the token to the
    // graveyard. REVERT-GUARD: without the keyword-then-quoted-ability fix the
    // quoted sacrifice ability is dropped, so the token survives the end step.
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    // REVERT-GUARD: the granted "At the beginning of the end step, sacrifice
    // this token" ability must fire and sacrifice the copy token. A token that
    // moves to the graveyard ceases to exist (CR 111.8 + CR 704.5d), so the
    // discriminating observation is that the token is no longer on the
    // battlefield. Without the keyword-then-quoted-ability parser fix (or the
    // token-copy GrantTrigger runtime application), the quoted sacrifice ability
    // is dropped and the copy token survives the end step on the battlefield.
    let state = runner.state();
    let token_still_on_battlefield = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .any(|o| o.id == token_id);
    assert!(
        !token_still_on_battlefield,
        "CR 707.9a: the granted end-step sacrifice ability must sacrifice the copy token \
         (token must have left the battlefield)"
    );
    // The token object ceased to exist as a graveyard token (CR 111.8).
    assert!(
        !state.objects.contains_key(&token_id)
            || state.objects.get(&token_id).map(|o| o.zone) != Some(Zone::Battlefield),
        "the sacrificed copy token must not remain a battlefield object"
    );
}

/// CR 707.9a + CR 604.1: a token copy whose except clause grants a quoted
/// static ability must carry that static onto the copy as part of its copiable
/// values, so the granted static immediately affects the objects it describes.
#[test]
fn copy_token_except_quoted_static_grants_static_ability() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario
        .add_creature(P0, "Bear", 3, 3)
        .with_subtypes(vec!["Bear"])
        .id();

    let sorcery = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic Static Copier",
            false,
            "Create a token that's a copy of target creature you control, except it has \
             \"Other Bears you control get +1/+1.\"",
        )
        .id();

    let mut runner = scenario.build();
    runner.cast(sorcery).target_object(source).resolve();

    let state = runner.state();
    let source_obj = state.objects.get(&source).expect("source Bear exists");
    assert_eq!(
        source_obj.power,
        Some(4),
        "CR 707.9a: the token's quoted static ability must grant +1/+1 to other Bears"
    );
    assert_eq!(
        source_obj.toughness,
        Some(4),
        "CR 707.9a: the token's quoted static ability must grant +1/+1 to other Bears"
    );

    let token = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|o| o.is_token && o.name == "Bear")
        .expect("copy token exists");
    assert!(
        !token.static_definitions.is_empty(),
        "CR 707.9a: the quoted static must be stamped onto the copy token"
    );
}

/// CR 707.9a + CR 707.2: a token copy whose except clause is a comma-anded body
/// list ending in ", and has <keyword>" must apply every body — including the
/// trailing keyword — so the token is non-legendary, gains the added subtype,
/// AND has the trailing keyword (haste).
#[test]
fn copy_token_except_comma_and_keyword_body_applies_trailing_keyword() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Copy source: a LEGENDARY 3/3 P0 controls, so the "isn't legendary" body is
    // observable on the token (the source keeps Legendary; the copy loses it).
    let source = scenario
        .add_creature(P0, "Legend Bear", 3, 3)
        .as_legendary()
        .id();

    let sorcery = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synthetic Reflection Copier",
            false,
            "Create a token that's a copy of target creature you control, except it isn't \
             legendary, is a Reflection in addition to its other types, and has haste.",
        )
        .id();

    let mut runner = scenario.build();
    runner.cast(sorcery).target_object(source).resolve();

    let state = runner.state();
    let tokens: Vec<_> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.is_token && o.name == "Legend Bear")
        .collect();
    assert_eq!(tokens.len(), 1, "exactly one copy token must be created");
    let token = tokens[0];

    // Each except body must have applied:
    assert!(
        !token.card_types.supertypes.contains(&Supertype::Legendary),
        "CR 707.9a: the copy token must NOT be legendary (\"except it isn't legendary\")"
    );
    assert!(
        token.card_types.subtypes.iter().any(|s| s == "Reflection"),
        "CR 707.9a: the copy token must gain the Reflection subtype"
    );
    // REVERT-GUARD: without the comma-splitter except-continuation fix, "and has
    // haste" is orphaned as an Unimplemented sub_ability and the token lacks
    // haste — this assertion fails.
    assert!(
        object_has_effective_keyword_kind(state, token.id, KeywordKind::Haste),
        "CR 707.9a: the trailing \", and has haste\" except body must grant haste to the token"
    );
}

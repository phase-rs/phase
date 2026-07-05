//! Standard small parser-only batch A — three parser seams that route to
//! existing engine surface, no new engine variant:
//!
//! - §16 mana-of-any-type spend permission (Vizier of the Menagerie SHIPPED —
//!   spell-class-filtered `SpendManaAsAnyColor { spell_filter }`; Outrageous
//!   Robbery SHIPPED — subject-voice exile-top-X face down + impulse-exile play
//!   grant with the "any type" mana rider folded onto it).
//! - §19 Role token attach (Royal Treatment SHIPPED, Become Brutes
//!   HONEST-DEFER — per-target iteration over the multi-target set).
//! - §22 "Otherwise" sequence-branch (Wick, the Whorled Mind SHIPPED, Bre of
//!   Clan Stoutarm SHIPPED — spell-MV-vs-life-gained comparison gate re-homed
//!   onto the cast clause so "Otherwise" routes to `else_ability`).
//!
//! The SHIPPED cards each carry a discriminating test that drives the
//! production path (`resolve_ability_chain` or the cast cost-payment path) and
//! asserts an outcome that FLIPS if the fix is reverted. The deferred cards
//! carry honesty guards asserting the residual gap is exactly the unsupported
//! clause (not an over-claim).

use engine::game::ability_utils::{build_resolved_from_def, build_resolved_from_def_with_targets};
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityCondition, Effect, TargetRef};
use engine::types::phase::Phase;

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
    let dbg = parsed_debug(oracle, name, types, subtypes);
    assert!(
        !dbg.contains("Unimplemented"),
        "{name}: expected zero Unimplemented nodes, parse was:\n{dbg}"
    );
}

// ===========================================================================
// §22 — Wick, the Whorled Mind (SHIPPED)
//
// "Whenever Wick or another Rat you control enters, create a 1/1 black Snail
//  creature token if you don't control a Snail. Otherwise, put a +1/+1 counter
//  on a Snail you control."
//
// Two parser bugs blocked the Otherwise routing:
//   1. `condition_text_is_rehomeable` excluded "you don't control a Snail"
//      because the reflexive-predicate prefix "you do" matched "you do**n't**"
//      (no word boundary), so the trailing "if" condition was never extracted.
//   2. the bare "you don't" reflexive arm in
//      `try_nom_condition_as_ability_condition` swallowed the control-presence
//      condition before the typed `parse_inner_condition` fall-through.
// With the Token clause carrying no condition, the Otherwise lowered to the
// unconditional `OtherwiseFallback` (`Unimplemented{otherwise}` + sibling
// counter) instead of attaching as the Token's `else_ability`.
// ===========================================================================

#[test]
fn wick_otherwise_attaches_as_token_else_ability() {
    const ORACLE: &str = "Whenever Wick or another Rat you control enters, create a 1/1 black Snail creature token if you don't control a Snail. Otherwise, put a +1/+1 counter on a Snail you control.\n{U}{B}{R}, Sacrifice a Snail: Wick deals damage equal to the sacrificed creature's power to each opponent. Then draw cards equal to the sacrificed creature's power.";
    assert_zero_unimplemented(
        ORACLE,
        "Wick, the Whorled Mind",
        &creature_types(),
        &["Rat".to_string()],
    );

    let parsed = parse_oracle_text(
        ORACLE,
        "Wick, the Whorled Mind",
        &[],
        &creature_types(),
        &["Rat".to_string()],
    );
    let trigger = &parsed.triggers[0];
    let execute = trigger
        .execute
        .as_deref()
        .expect("Wick ETB trigger must carry an execute ability");
    // Revert guard (shape): the conditional must be a Token gated by a
    // control-presence QuantityCheck with the PutCounter as its else_ability.
    // Pre-fix this lowered to Token (no condition) → Unimplemented{otherwise} →
    // PutCounter sibling.
    assert!(
        matches!(*execute.effect, Effect::Token { .. }),
        "execute root must be the Token creation, got {:?}",
        execute.effect
    );
    assert!(
        matches!(
            execute.condition,
            Some(AbilityCondition::QuantityCheck { .. })
        ),
        "Token clause must carry the 'you don't control a Snail' gate, got {:?}",
        execute.condition
    );
    let else_branch = execute
        .else_ability
        .as_deref()
        .expect("Otherwise must attach the PutCounter as else_ability, not a fallback sibling");
    assert!(
        matches!(*else_branch.effect, Effect::PutCounter { .. }),
        "else branch must be the +1/+1 counter, got {:?}",
        else_branch.effect
    );

    // Runtime discrimination on the CONDITION GATE through the production
    // resolver. The fix routes "you don't control a Snail" onto the Token's
    // `condition`; pre-fix the Token carried no condition (Otherwise lowered to
    // an unconditional fallback), so the token would be created unconditionally.
    //
    // Case A — no Snail controlled (condition TRUE): the if-branch fires and a
    // Snail token IS created.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let wick = scenario
            .add_creature(P0, "Wick, the Whorled Mind", 0, 3)
            .id();
        let mut runner = scenario.build();
        let snails_before = count_snails(&runner);
        let ability = build_resolved_from_def(execute, wick, P0);
        let mut events = Vec::new();
        resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
            .expect("Wick ETB if-branch must resolve");
        assert_eq!(
            count_snails(&runner),
            snails_before + 1,
            "with no Snail controlled, the if-branch must create a Snail token"
        );
    }

    // Case B — a Snail already controlled (condition FALSE): the if-branch is
    // gated OFF, so NO new Snail token is created (the else branch runs instead).
    // This is the assertion that FLIPS on revert: pre-fix the token was created
    // regardless of the condition.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let wick = scenario
            .add_creature(P0, "Wick, the Whorled Mind", 0, 3)
            .id();
        let existing_snail = scenario
            .add_creature(P0, "Resident Snail", 1, 1)
            .with_subtypes(vec!["Snail"])
            .id();
        let mut runner = scenario.build();
        let snails_before = count_snails(&runner);
        let ability = build_resolved_from_def(execute, wick, P0);
        let mut events = Vec::new();
        resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
            .expect("Wick ETB else-branch must resolve");
        assert_eq!(
            count_snails(&runner),
            snails_before,
            "with a Snail already controlled, the if-branch (token) must be gated off"
        );
        assert_eq!(
            runner
                .state()
                .objects
                .get(&existing_snail)
                .and_then(|o| o
                    .counters
                    .get(&engine::types::counter::CounterType::Plus1Plus1))
                .copied()
                .unwrap_or(0),
            1,
            "the else branch must put a +1/+1 counter on the controlled Snail"
        );
    }
}

fn count_snails(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.card_types.subtypes.iter().any(|s| s == "Snail"))
        .count()
}

// ===========================================================================
// §19 — Royal Treatment (SHIPPED)
//
// "Target creature you control gains hexproof until end of turn. Create a Royal
//  Role token attached to that creature."
//
// Bug: the card-name first-word fallback in `normalize_card_name_refs` replaced
// "Royal" (first word of "Royal Treatment") with `~` inside the token name
// "Royal Role", corrupting it to "~ Role" — the token parser then failed and
// the clause stayed an `Unimplemented{create}`. Fixed by guarding the fallback
// against a card-name word immediately followed by a token-subtype noun
// ("Role"/"Aura").
// ===========================================================================

#[test]
fn royal_treatment_creates_role_token_attached_to_target() {
    const ORACLE: &str = "Target creature you control gains hexproof until end of turn. Create a Royal Role token attached to that creature. (If you control another Role on it, put that one into the graveyard. Enchanted creature gets +1/+1 and has ward {1}.)";
    assert_zero_unimplemented(ORACLE, "Royal Treatment", &["Instant".to_string()], &[]);

    let parsed = parse_oracle_text(
        ORACLE,
        "Royal Treatment",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let spell = &parsed.abilities[0];
    // Revert guard (shape): the Role token clause must reach a Token effect with
    // the correct (un-mangled) token name. Pre-fix the name was "~ Role" and the
    // clause was Unimplemented{create}.
    let token_def = {
        // Walk the sub_ability chain for the Token clause (the hexproof grant is
        // the root; the Role token rides in the sub_ability chain).
        let mut found = None;
        let mut cur = spell.sub_ability.as_deref();
        while let Some(def) = cur {
            if matches!(*def.effect, Effect::Token { .. }) {
                found = Some(def);
                break;
            }
            cur = def.sub_ability.as_deref();
        }
        found.expect("Royal Treatment must lower a Role Token creation clause")
    };
    let Effect::Token {
        name, attach_to, ..
    } = token_def.effect.as_ref()
    else {
        unreachable!("token_def filtered to Effect::Token");
    };
    assert_eq!(
        name, "Royal Role",
        "token name must be the un-mangled 'Royal Role' (pre-fix it was '~ Role')"
    );
    assert!(
        attach_to.is_some(),
        "the Role token must carry an attach target ('attached to that creature')"
    );

    // Runtime discrimination: resolving the spell against a controlled creature
    // creates exactly one new permanent — the Royal Role token — attached to the
    // targeted creature. If reverted, the clause is Unimplemented and creates
    // nothing.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature(P0, "Royal Treatment Source", 1, 1)
        .id();
    let target = scenario.add_creature(P0, "Target Bear", 2, 2).id();
    let mut runner = scenario.build();
    let objects_before = runner.state().objects.len();

    let ability =
        build_resolved_from_def_with_targets(spell, source, P0, vec![TargetRef::Object(target)]);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Royal Treatment must resolve");

    let new_objects: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.name == "Royal Role")
        .collect();
    assert_eq!(
        new_objects.len(),
        1,
        "exactly one Royal Role token must be created"
    );
    assert!(
        runner.state().objects.len() > objects_before,
        "a new permanent (the Role token) must exist after resolution"
    );
    let role = new_objects[0];
    assert_eq!(
        role.attached_to
            .and_then(|attached_to| attached_to.as_object()),
        Some(target),
        "the Royal Role token must enter attached to the targeted creature (CR 303.7)"
    );
}

// ===========================================================================
// HONEST DEFERS — assert the residual gap is exactly the unsupported clause.
// ===========================================================================

// §16 — Vizier of the Menagerie. "You can spend mana of any type to cast
// creature spells." (CR 609.4b) lowers to a spell-class-filtered
// `SpendManaAsAnyColor { spell_filter: Some(creature) }` static, NOT the
// unfiltered global form — routing it to the unfiltered static would let
// any-type mana pay for NONcreature spells too (rules-wrong). All three lines
// now parse with zero Unimplemented nodes.
//
// Shape test only; the runtime discrimination (off-color mana pays a creature
// spell ONLY under this static and ONLY for creature spells) lives in
// `casting.rs::vizier_filtered_static_grants_any_type_mana_for_creature_spells`.
#[test]
fn vizier_filtered_any_type_spend_static_parses() {
    use engine::types::ability::{TargetFilter, TypeFilter};
    use engine::types::statics::StaticMode;

    const ORACLE: &str = "You may look at the top card of your library any time.\nYou may cast creature spells from the top of your library.\nYou can spend mana of any type to cast creature spells.";
    let parsed = parse_oracle_text(
        ORACLE,
        "Vizier of the Menagerie",
        &[],
        &creature_types(),
        &["Cat".to_string()],
    );

    let dbg = format!("{parsed:#?}");
    assert!(
        !dbg.contains("Unimplemented"),
        "Vizier must parse with zero Unimplemented nodes; got:\n{dbg}"
    );

    // The any-type-spend line must be the spell-class-FILTERED static (creature
    // spells), never the unfiltered board-wide `spell_filter: None` form.
    let spend_static = parsed
        .statics
        .iter()
        .find_map(|s| match &s.mode {
            StaticMode::SpendManaAsAnyColor { spell_filter, .. } => Some(spell_filter),
            _ => None,
        })
        .expect("the any-type-spend line must lower to a SpendManaAsAnyColor static");
    let filter = spend_static.as_ref().expect(
        "the static must be spell-class-FILTERED (Some), not the unfiltered board-wide form",
    );
    match filter {
        TargetFilter::Typed(typed) => assert!(
            typed.type_filters.contains(&TypeFilter::Creature),
            "the spell filter must scope to creature spells; got {filter:?}"
        ),
        other => panic!("expected a Typed(creature) spell filter, got {other:?}"),
    }

    // The other two statics must still parse.
    assert!(
        parsed
            .statics
            .iter()
            .any(|s| matches!(s.mode, StaticMode::MayLookAtTopOfLibrary)),
        "the look-at-top static must still parse"
    );
    assert!(
        parsed
            .statics
            .iter()
            .any(|s| matches!(s.mode, StaticMode::TopOfLibraryCastPermission { .. })),
        "the cast-creature-spells-from-top static must still parse"
    );
}

// §16 — Outrageous Robbery. Now supported: the subject-voice "target opponent
// exiles the top X cards of their library face down" lowers to
// `ExileTop { player: Opponent, count: Variable(X), face_down: true }`, the
// "look at and play those cards for as long as they remain exiled" clause to a
// `GrantCastingPermission { PlayFromExile }` over the tracked set, and the "if
// you cast a spell this way, you may spend mana as though it were mana of any
// type" rider folds onto that grant as `mana_spend_permission: AnyTypeOrColor`.
// Revert any of the three parser gaps and an assertion below flips:
//   * X-count fix reverted    → count is Fixed(1), not Variable(X)
//   * face-down fix reverted  → face_down is false
//   * "any type" scan reverted→ a residual Unimplemented reappears + no mana perm
#[test]
fn outrageous_robbery_look_and_play_any_type_parses() {
    use engine::types::ability::{
        CastingPermission, ManaSpendPermission, QuantityExpr, QuantityRef,
    };
    const ORACLE: &str = "Target opponent exiles the top X cards of their library face down. You may look at and play those cards for as long as they remain exiled. If you cast a spell this way, you may spend mana as though it were mana of any type to cast it.";
    let parsed = parse_oracle_text(
        ORACLE,
        "Outrageous Robbery",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let dbg = format!("{parsed:#?}");
    assert!(
        !dbg.contains("Unimplemented"),
        "Outrageous Robbery must have zero Unimplemented nodes, got:\n{dbg}"
    );

    let head = &parsed.abilities[0];
    match &*head.effect {
        Effect::ExileTop {
            count, face_down, ..
        } => {
            assert!(
                matches!(
                    count,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Variable { name }
                    } if name == "X"
                ),
                "exile count must be the cost's X, got {count:?}"
            );
            assert!(*face_down, "exiled cards must be face down (CR 406.3)");
        }
        other => panic!("expected ExileTop head, got {other:?}"),
    }

    let grant = head
        .sub_ability
        .as_ref()
        .expect("ExileTop must chain the play grant");
    match &*grant.effect {
        Effect::GrantCastingPermission {
            permission:
                CastingPermission::PlayFromExile {
                    mana_spend_permission,
                    ..
                },
            ..
        } => assert_eq!(
            *mana_spend_permission,
            Some(ManaSpendPermission::AnyTypeOrColor),
            "the 'spend mana as though any type' rider must fold onto the play grant"
        ),
        other => panic!("expected GrantCastingPermission sub-ability, got {other:?}"),
    }
}

// §19 — Become Brutes. "For each of those creatures, create a Monster Role
// token attached to it." Needs per-target iteration over the spell's chosen
// multi-target set with a per-iteration attach binding — infrastructure beyond
// the single-target Role-attach surface. The haste grant still parses.
#[test]
fn become_brutes_for_each_target_role_is_honestly_deferred() {
    const ORACLE: &str = "One or two target creatures each gain haste until end of turn. For each of those creatures, create a Monster Role token attached to it. (If you control another Role on it, put that one into the graveyard. Enchanted creature gets +1/+1 and has trample.)";
    let dbg = parsed_debug(ORACLE, "Become Brutes", &["Instant".to_string()], &[]);
    assert!(
        dbg.contains("Unimplemented")
            && dbg.contains("For each of those creatures, create a Monster Role token"),
        "the for-each-target Role clause must remain an honest Unimplemented defer; got:\n{dbg}"
    );
    assert!(
        dbg.contains("Haste"),
        "the 'each gain haste' grant must still parse"
    );
}

// §22 — Bre of Clan Stoutarm (SHIPPED). The spell-MV-vs-dynamic-life-gained
// gate "if the spell's mana value is less than or equal to the amount of life
// you gained this turn" is now re-homed onto the cast clause as a
// `QuantityCheck` condition (`ObjectManaValue { Target } <= LifeGainedThisTurn
// { Controller }`), which in turn routes "Otherwise, put it into your hand" to
// the cast's `else_ability` (`ChangeZone -> Hand`) instead of an Unimplemented
// fallback. Runtime coverage: `bre_of_clan_stoutarm_endstep.rs`.
#[test]
fn bre_otherwise_mv_vs_life_gate_parses_to_conditional_cast() {
    const ORACLE: &str = "{1}{W}, {T}: Another target creature you control gains flying and lifelink until end of turn.\nAt the beginning of your end step, if you gained life this turn, exile cards from the top of your library until you exile a nonland card. You may cast that card without paying its mana cost if the spell's mana value is less than or equal to the amount of life you gained this turn. Otherwise, put it into your hand.";
    let dbg = parsed_debug(
        ORACLE,
        "Bre of Clan Stoutarm",
        &creature_types(),
        &["Dwarf".to_string(), "Warrior".to_string()],
    );
    // The gate is modeled, so the whole card is now free of Unimplemented nodes.
    assert!(
        !dbg.contains("Unimplemented"),
        "Bre's gate is now modeled; expected zero Unimplemented nodes, got:\n{dbg}"
    );
    // The exile-until body, the gated free cast, and the else->hand branch.
    assert!(
        dbg.contains("ExileFromTopUntil") && dbg.contains("CastFromZone"),
        "exile-until + cast must parse:\n{dbg}"
    );
    assert!(
        dbg.contains("ObjectManaValue") && dbg.contains("LifeGainedThisTurn"),
        "cast clause must carry the MV<=life-gained gate:\n{dbg}"
    );
    assert!(
        dbg.contains("else_ability") && dbg.contains("ChangeZone"),
        "Otherwise must route to else_ability ChangeZone->Hand:\n{dbg}"
    );
}

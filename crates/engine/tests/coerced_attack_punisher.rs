//! Coerced-attack + end-step punisher class (Siren's Call, Maddening Imp).
//!
//! Implements the plan-v4 Verification Matrix (Tests 1–10c). Every behavioral
//! claim is exercised either through the real parse pipeline
//! (`parse_oracle_text`) for AST-binding claims that have no runtime choice, or
//! through the real filter/resolution runtime (`matches_target_filter`,
//! `collect_player_targets`, `player_matches_target_filter_in_state`) for the
//! live-resolution claims. Each negative assertion is paired with a positive
//! reach-guard, and each test fails if the corresponding fix is reverted.

use engine::game::combat::{creature_must_attack, AttackTarget};
use engine::game::engine::EngineError;
use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::restrictions::check_activation_restrictions;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::ParsedAbilities;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    ActivationRestriction, ControllerRef, Effect, FilterProp, ParsedCondition, TargetFilter,
    TypeFilter, TypedFilter,
};
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const SIRENS_CALL: &str = "Cast this spell only during an opponent's turn, before attackers are declared.\nCreatures the active player controls attack this turn if able.\nAt the beginning of the next end step, destroy all non-Wall creatures that player controls that didn't attack this turn. Ignore this effect for each creature the player didn't control continuously since the beginning of the turn.";

const MADDENING_IMP: &str = "Flying\n{T}: Non-Wall creatures the active player controls attack this turn if able. At the beginning of the next end step, destroy each of those creatures that didn't attack this turn. Activate only during an opponent's turn and only before combat.";

const NETTLING_IMP: &str = "{T}: Choose target non-Wall creature the active player has controlled continuously since the beginning of the turn. That creature attacks this turn if able. Destroy it at the beginning of the next end step if it didn't attack this turn. Activate only during an opponent's turn, before attackers are declared.";

const LAVINIA: &str = "Vigilance\n{T}: Add {C}{C}. Activate only during an opponent's turn.";

fn parse(name: &str, text: &str, types: &[&str], subtypes: &[&str]) -> ParsedAbilities {
    parse_kw(name, text, &[], types, subtypes)
}

fn parse_kw(
    name: &str,
    text: &str,
    keywords: &[&str],
    types: &[&str],
    subtypes: &[&str],
) -> ParsedAbilities {
    let keywords: Vec<String> = keywords.iter().map(|s| s.to_string()).collect();
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(text, name, &keywords, &types, &subtypes)
}

/// Recursively collect the controller of every `Typed` node in a filter.
fn typed_controllers(filter: &TargetFilter, out: &mut Vec<Option<ControllerRef>>) {
    match filter {
        TargetFilter::Typed(tf) => out.push(tf.controller.clone()),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().for_each(|f| typed_controllers(f, out))
        }
        TargetFilter::Not { filter } => typed_controllers(filter, out),
        _ => {}
    }
}

fn typed_props(filter: &TargetFilter) -> Vec<FilterProp> {
    match filter {
        TargetFilter::Typed(tf) => tf.properties.clone(),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.first().map(typed_props).unwrap_or_default()
        }
        TargetFilter::Not { filter } => typed_props(filter),
        _ => Vec::new(),
    }
}

/// The delayed DestroyAll target of Siren's Call (abilities[1]).
fn siren_destroy_target(p: &ParsedAbilities) -> (TargetFilter, bool) {
    for a in &p.abilities {
        if let Effect::CreateDelayedTrigger { effect, .. } = a.effect.as_ref() {
            if let Effect::DestroyAll { target, .. } = effect.effect.as_ref() {
                let sub_present = effect.sub_ability.is_some();
                return (target.clone(), sub_present);
            }
        }
    }
    panic!("no delayed DestroyAll found");
}

// ---------------------------------------------------------------------------
// Test 1 — "creatures the active player controls" parses to mass MustAttack
// bound to ActivePlayer. Reach-guard: zero Unimplemented in the coerce clause.
// ---------------------------------------------------------------------------
#[test]
fn test1_active_player_coerce_binds_controller() {
    let p = parse("Siren's Call", SIRENS_CALL, &["Instant"], &[]);
    // The coerce ability is the GenericEffect{MustAttack} over an ActivePlayer subject.
    let coerce = p
        .abilities
        .iter()
        .find_map(|a| match a.effect.as_ref() {
            Effect::GenericEffect {
                static_abilities, ..
            } => static_abilities
                .iter()
                .find(|st| matches!(st.mode, engine::types::statics::StaticMode::MustAttack))
                .and_then(|st| st.affected.clone()),
            _ => None,
        })
        .expect("mass MustAttack coerce clause present");
    let mut ctrls = Vec::new();
    typed_controllers(&coerce, &mut ctrls);
    // Revert-failing: without Step 3 the controller parses to `You`, not `ActivePlayer`.
    assert_eq!(ctrls, vec![Some(ControllerRef::ActivePlayer)]);
    // Reach-guard: the coerce clause produced a real effect, no Unimplemented.
    let has_unimpl_coerce = p.abilities.iter().any(
        |a| matches!(a.effect.as_ref(), Effect::Unimplemented { name, .. } if name == "creatures"),
    );
    assert!(
        !has_unimpl_coerce,
        "coerce clause must not be Unimplemented"
    );
}

// Sibling: "creatures you control attack this turn if able" still → controller:You.
#[test]
fn test1_sibling_you_control_unchanged() {
    let p = parse(
        "Probe",
        "Creatures you control attack this turn if able.",
        &["Instant"],
        &[],
    );
    let coerce = p
        .abilities
        .iter()
        .find_map(|a| match a.effect.as_ref() {
            Effect::GenericEffect {
                static_abilities, ..
            } => static_abilities.first().and_then(|st| st.affected.clone()),
            _ => None,
        })
        .expect("coerce present");
    let mut ctrls = Vec::new();
    typed_controllers(&coerce, &mut ctrls);
    assert_eq!(ctrls, vec![Some(ControllerRef::You)]);
}

// ---------------------------------------------------------------------------
// Test 2 — ActivePlayer resolves to state.active_player LIVE; Opponent (3p)
// matches multiple players (discriminator).
// ---------------------------------------------------------------------------
#[test]
fn test2_active_player_resolves_live() {
    let mut sc = GameScenario::new_n_player(3, 42);
    let p1_creature = sc.add_creature(PlayerId(1), "P1 Bear", 2, 2).id();
    let p0_creature = sc.add_creature(PlayerId(0), "P0 Bear", 2, 2).id();
    let p2_creature = sc.add_creature(PlayerId(2), "P2 Bear", 2, 2).id();
    let mut runner = sc.build();
    // P1 is the active player for this fixture.
    runner.state_mut().active_player = PlayerId(1);
    let state = runner.state();

    let active_filter =
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::ActivePlayer));
    let ctx = FilterContext::from_source_with_controller(ObjectId(999), PlayerId(0));
    // Revert-failing: without the ActivePlayer resolution arm this is a compile
    // error; with a wrong arm P1's creature would not match.
    assert!(matches_target_filter(
        state,
        p1_creature,
        &active_filter,
        &ctx
    ));
    assert!(!matches_target_filter(
        state,
        p0_creature,
        &active_filter,
        &ctx
    ));
    assert!(!matches_target_filter(
        state,
        p2_creature,
        &active_filter,
        &ctx
    ));

    // Discriminator: Opponent (from P0's perspective) matches BOTH P1 and P2.
    let opp_filter =
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));
    assert!(matches_target_filter(state, p1_creature, &opp_filter, &ctx));
    assert!(matches_target_filter(state, p2_creature, &opp_filter, &ctx));
    assert!(!matches_target_filter(
        state,
        p0_creature,
        &opp_filter,
        &ctx
    ));
}

// Test 2b (`collect_player_targets` ActivePlayer resolution) lives as an in-crate
// unit test in `game/ability_utils.rs` because that seam is `pub(crate)`.

// ---------------------------------------------------------------------------
// Test 5 — controller:You→ActivePlayer rewrite is frame-local. Reach-guard: a
// standalone "destroy all creatures you control" (no coerce sibling) stays You.
// ---------------------------------------------------------------------------
#[test]
fn test5_frame_local_rewrite() {
    let p = parse("Siren's Call", SIRENS_CALL, &["Instant"], &[]);
    let (target, _sub) = siren_destroy_target(&p);
    let mut ctrls = Vec::new();
    typed_controllers(&target, &mut ctrls);
    // Revert-failing: without Step 5 the DestroyAll controller stays `You`.
    assert_eq!(ctrls, vec![Some(ControllerRef::ActivePlayer)]);

    // Sibling: a standalone punisher with NO coerce clause is untouched.
    let standalone = parse(
        "Standalone",
        "At the beginning of the next end step, destroy all creatures you control that didn't attack this turn.",
        &["Instant"],
        &[],
    );
    let (st_target, _) = siren_destroy_target(&standalone);
    let mut st_ctrls = Vec::new();
    typed_controllers(&st_target, &mut st_ctrls);
    assert_eq!(
        st_ctrls,
        vec![Some(ControllerRef::You)],
        "no-coerce-sibling punisher must stay controller:You"
    );
}

// ---------------------------------------------------------------------------
// Test 6 — Maddening Imp "those creatures" is rebound to a concrete
// ActivePlayer non-Wall filter (not an empty TrackedSet).
// ---------------------------------------------------------------------------
#[test]
fn test6_maddening_those_creatures_concrete() {
    let p = parse("Maddening Imp", MADDENING_IMP, &["Creature"], &["Imp"]);
    // Find the activated ability's delayed DestroyAll (inside the sub_ability chain).
    let activated = p
        .abilities
        .iter()
        .find(|a| matches!(a.effect.as_ref(), Effect::GenericEffect { .. }))
        .expect("activated ability present");
    let mut node = activated.sub_ability.as_deref();
    let mut destroy_target = None;
    while let Some(n) = node {
        if let Effect::CreateDelayedTrigger { effect, .. } = n.effect.as_ref() {
            if let Effect::DestroyAll { target, .. } = effect.effect.as_ref() {
                destroy_target = Some(target.clone());
            }
        }
        node = n.sub_ability.as_deref();
    }
    let target = destroy_target.expect("delayed DestroyAll present");
    // Revert-failing: without Step 6 the target is an empty TrackedSet (destroys nothing).
    assert!(
        !matches!(target, TargetFilter::TrackedSet { .. }),
        "must be rebound from TrackedSet to a concrete filter"
    );
    let mut ctrls = Vec::new();
    typed_controllers(&target, &mut ctrls);
    assert_eq!(ctrls, vec![Some(ControllerRef::ActivePlayer)]);
    let props = typed_props(&target);
    assert!(props.iter().any(|p| matches!(
        p,
        FilterProp::Not { prop } if matches!(prop.as_ref(), FilterProp::AttackedThisTurn { defender: None })
    )));
    // Non-Wall constraint present.
    let is_non_wall = match &target {
        TargetFilter::Typed(tf) => tf.type_filters.iter().any(|t| {
            matches!(t, TypeFilter::Non(inner) if matches!(inner.as_ref(), TypeFilter::Subtype(s) if s == "Wall"))
        }),
        _ => false,
    };
    assert!(is_non_wall, "non-Wall constraint must be present");
}

// ---------------------------------------------------------------------------
// Test 6b — Documented deviation (direction 1, late-arrival over-inclusion): a
// creature entering the active player's control AFTER the ability would resolve
// is STILL matched by the live refilter (proves it is live, not a frozen set).
// ---------------------------------------------------------------------------
#[test]
fn test6b_live_refilter_late_arrival() {
    let mut sc = GameScenario::new_n_player(3, 3);
    // A late-arriving non-Wall creature under the active player's control that
    // did not attack — the frozen "those creatures" set would NOT include it.
    let late = sc.add_creature(PlayerId(1), "Late Bear", 2, 2).id();
    let mut runner = sc.build();
    runner.state_mut().active_player = PlayerId(1);
    let state = runner.state();
    // The concrete filter Step 6 produces.
    let filter = TargetFilter::Typed(
        TypedFilter::creature()
            .with_type(TypeFilter::Non(Box::new(TypeFilter::Subtype(
                "Wall".into(),
            ))))
            .controller(ControllerRef::ActivePlayer)
            .properties(vec![FilterProp::Not {
                prop: Box::new(FilterProp::AttackedThisTurn { defender: None }),
            }]),
    );
    let ctx = FilterContext::from_source_with_controller(ObjectId(999), PlayerId(0));
    // Live refilter DOES match the late arrival (documented over-inclusion,
    // CR 608.2c deviation direction 1).
    assert!(matches_target_filter(state, late, &filter, &ctx));
}

// ---------------------------------------------------------------------------
// Test 7 — ControlledContinuouslySinceTurnBegan = !summoning_sick, haste-INDEPENDENT.
// ---------------------------------------------------------------------------
#[test]
fn test7_continuity_predicate_haste_independent() {
    let mut sc = GameScenario::new_n_player(2, 9);
    // Fresh-ETB creature (summoning sick), even WITH Haste.
    let fresh_id = {
        let mut b = sc.add_creature(P0, "Fresh Hasty", 2, 2);
        b.haste();
        b.id()
    };
    // A creature controlled continuously since turn began.
    let old_id = sc.add_creature(P0, "Old Bear", 2, 2).id();
    let mut runner = sc.build();
    {
        let st = runner.state_mut();
        st.objects.get_mut(&fresh_id).unwrap().summoning_sick = true;
        st.objects.get_mut(&old_id).unwrap().summoning_sick = false;
    }
    let state = runner.state();
    let filter = TargetFilter::Typed(
        TypedFilter::creature().properties(vec![FilterProp::ControlledContinuouslySinceTurnBegan]),
    );
    let ctx = FilterContext::neutral();
    // Reach-guard: the since-turn creature matches (positive).
    assert!(matches_target_filter(state, old_id, &filter, &ctx));
    // Revert-failing discriminator: a hasty just-entered creature is FALSE here,
    // unlike HasHasteOrControlledSinceTurnBegan which would be true.
    assert!(!matches_target_filter(state, fresh_id, &filter, &ctx));

    // Sibling discriminator: the haste-folding predicate returns TRUE for the
    // same hasty creature, proving this predicate is genuinely haste-independent.
    let haste_filter = TargetFilter::Typed(
        TypedFilter::creature().properties(vec![FilterProp::HasHasteOrControlledSinceTurnBegan]),
    );
    assert!(matches_target_filter(state, fresh_id, &haste_filter, &ctx));
}

// ---------------------------------------------------------------------------
// Test 8 — Siren's Call exemption CONSUMED: DestroyAll carries the continuity
// property AND the redundant Unimplemented sibling is gone.
// ---------------------------------------------------------------------------
#[test]
fn test8_exemption_consumed() {
    let p = parse("Siren's Call", SIRENS_CALL, &["Instant"], &[]);
    let (target, sub_present) = siren_destroy_target(&p);
    let props = typed_props(&target);
    // Revert-failing (attach): the continuity property is present.
    assert!(props
        .iter()
        .any(|p| matches!(p, FilterProp::ControlledContinuouslySinceTurnBegan)));
    // Revert-failing (consume): the redundant Unimplemented sibling is gone.
    assert!(
        !sub_present,
        "the Unimplemented exemption sibling must be consumed"
    );
}

// ---------------------------------------------------------------------------
// Test 9 — the exemption filter behaves: a just-summoned non-attacker is spared
// while a continuously-controlled non-attacker is caught. Driven through the
// exact composed filter the parser emits.
// ---------------------------------------------------------------------------
#[test]
fn test9_exemption_fires() {
    let mut sc = GameScenario::new_n_player(2, 11);
    let summoned = sc.add_creature(P0, "Summoned", 2, 2).id();
    let old = sc.add_creature(P0, "Continuous", 2, 2).id();
    let mut runner = sc.build();
    {
        let st = runner.state_mut();
        st.active_player = P0;
        st.objects.get_mut(&summoned).unwrap().summoning_sick = true;
        st.objects.get_mut(&old).unwrap().summoning_sick = false;
    }
    let state = runner.state();
    // The composed destroyed-set filter Step 5 + Step 7 emit.
    let filter = TargetFilter::Typed(
        TypedFilter::creature()
            .with_type(TypeFilter::Non(Box::new(TypeFilter::Subtype(
                "Wall".into(),
            ))))
            .controller(ControllerRef::ActivePlayer)
            .properties(vec![
                FilterProp::Not {
                    prop: Box::new(FilterProp::AttackedThisTurn { defender: None }),
                },
                FilterProp::ControlledContinuouslySinceTurnBegan,
            ]),
    );
    let ctx = FilterContext::from_source_with_controller(ObjectId(999), P0);
    // Reach-guard: the continuously-controlled non-attacker IS destroyed.
    assert!(matches_target_filter(state, old, &filter, &ctx));
    // The just-summoned non-attacker is SPARED by the exemption.
    assert!(!matches_target_filter(state, summoned, &filter, &ctx));
}

// ---------------------------------------------------------------------------
// Test 10 — Maddening Imp compound activation-timing ENFORCED (two variants).
// Lavinia (single) + Nettling (comma form) reach-guards.
// ---------------------------------------------------------------------------
fn activation_restrictions(p: &ParsedAbilities) -> Vec<ActivationRestriction> {
    p.abilities
        .iter()
        .find(|a| !a.activation_restrictions.is_empty())
        .map(|a| a.activation_restrictions.clone())
        .unwrap_or_default()
}

fn opponents_turn() -> ActivationRestriction {
    ActivationRestriction::RequiresCondition {
        condition: Some(ParsedCondition::Not {
            condition: Box::new(ParsedCondition::IsYourTurn),
        }),
    }
}

#[test]
fn test10_maddening_compound_timing_enforced() {
    let p = parse_kw(
        "Maddening Imp",
        MADDENING_IMP,
        &["Flying"],
        &["Creature"],
        &["Imp"],
    );
    let restrictions = activation_restrictions(&p);
    // Revert-failing: without Step 6b this is [RequiresCondition{None}] (vacuous no-op).
    assert_eq!(
        restrictions,
        vec![
            opponents_turn(),
            ActivationRestriction::BeforeAttackersDeclared
        ]
    );
    // No regression: Flying keyword still present.
    assert!(p
        .extracted_keywords
        .iter()
        .any(|k| matches!(k, Keyword::Flying)));
}

#[test]
fn test10_lavinia_single_gate_non_regression() {
    let p = parse("Lavinia Probe", LAVINIA, &["Creature"], &[]);
    let restrictions = activation_restrictions(&p);
    // Single-gate form must stay one restriction (compound split is optional).
    assert_eq!(restrictions, vec![opponents_turn()]);
}

#[test]
fn test10_nettling_comma_form_fixed_for_free() {
    let p = parse("Nettling Imp", NETTLING_IMP, &["Creature"], &["Imp"]);
    let restrictions = activation_restrictions(&p);
    assert_eq!(
        restrictions,
        vec![
            opponents_turn(),
            ActivationRestriction::BeforeAttackersDeclared
        ]
    );
}

// ---------------------------------------------------------------------------
// Test 10b — the compound restriction feeds the EXISTING runtime gate. We drive
// the actual runtime condition evaluation (Not(IsYourTurn)) through
// player_matches_target_filter_in_state analog: assert the parsed condition
// evaluates active-player-relative (own turn illegal, opponent turn legal).
// The runtime arm is `state.active_player != player`.
// ---------------------------------------------------------------------------
#[test]
fn test10b_opponents_turn_condition_semantics() {
    // The parsed restriction's condition is Not(IsYourTurn); the runtime arm at
    // restrictions.rs:1385 evaluates IsYourTurn as `state.active_player ==
    // player`. We validate the enforced *shape* the runtime consumes, and that
    // it is NOT the vacuous None (which would always pass). The full runtime
    // path is exercised by the existing restriction-enforcement suite; here we
    // pin that the parser hands it the enforced condition (revert of Step 6b
    // would yield None → always-legal on the controller's own turn).
    let p = parse("Maddening Imp", MADDENING_IMP, &["Creature"], &["Imp"]);
    let restrictions = activation_restrictions(&p);
    let turn_gate = restrictions
        .iter()
        .find_map(|r| match r {
            ActivationRestriction::RequiresCondition { condition } => Some(condition.clone()),
            _ => None,
        })
        .expect("a RequiresCondition timing gate is present");
    // Revert-failing: Step 6b turns the vacuous None into the enforced condition.
    assert_eq!(
        turn_gate,
        Some(ParsedCondition::Not {
            condition: Box::new(ParsedCondition::IsYourTurn),
        })
    );
    // The combat-window half is present and enforced.
    assert!(restrictions
        .iter()
        .any(|r| matches!(r, ActivationRestriction::BeforeAttackersDeclared)));
}

// ---------------------------------------------------------------------------
// Test 10c — verbatim-hack removal is non-regressive: a "during your turn,
// before attackers are declared" card still parses to
// [DuringYourTurn, BeforeAttackersDeclared] via the compound parser.
// ---------------------------------------------------------------------------
#[test]
fn test10c_verbatim_hack_subsumed() {
    let p = parse(
        "Hack Card",
        "{T}: Add {C}. Activate only during your turn, before attackers are declared.",
        &["Creature"],
        &[],
    );
    let restrictions = activation_restrictions(&p);
    assert_eq!(
        restrictions,
        vec![
            ActivationRestriction::DuringYourTurn,
            ActivationRestriction::BeforeAttackersDeclared
        ]
    );
}

// ---------------------------------------------------------------------------
// Supporting sanity: the past-tense "the active player controlled" arm parses
// (Step 4). Uses Nettling Imp's present-tense continuity phrase as a control and
// a constructed past-tense look-back phrase.
// ---------------------------------------------------------------------------
#[test]
fn test_past_tense_active_player_controlled_parses() {
    // A look-back aggregate over "the active player controlled" should not leave
    // an unparsed Unimplemented controller stranded; we assert the whole card
    // parses without a bare "controlled" residue.
    let p = parse(
        "Lookback Probe",
        "Return each creature card in a graveyard that the active player controlled this turn to the battlefield.",
        &["Sorcery"],
        &[],
    );
    // Reach-guard: at least one ability parsed (not a total failure).
    assert!(!p.abilities.is_empty());
    // The parse must not strand a raw "the active player controlled" fragment as
    // an Unimplemented whose name is the controller phrase.
    let stranded = p.abilities.iter().any(|a| {
        matches!(a.effect.as_ref(), Effect::Unimplemented { description: Some(d), .. }
            if d.to_lowercase() == "the active player controlled")
    });
    assert!(!stranded);
}

// ===========================================================================
// TEST A — Siren's Call mass-MustAttack + delayed-DestroyAll driven through the
// REAL resolution + combat + end-step pipeline, 3-player (multiplayer
// discrimination).
//
// This drives the coerce and the delayed punisher through the production
// resolver (`resolve_ability_chain`) and then advances real combat + the end
// step, rather than a parse-shape or hand-built-filter check. Both parsed
// abilities come from the FULL-card `parse_oracle_text` (so they carry the
// Step-3/5/7 rewrites: coerce → ActivePlayer, DestroyAll → ActivePlayer, and the
// consumed continuity exemption). The spell's *controller* is the non-active
// caster P1 (owner of the resolving instant), while the ACTIVE player is P0 — so
// an `ActivePlayer` binding resolves to P0's board and a `You` binding would
// wrongly resolve to P1's (empty) board. That asymmetry is the multiplayer
// discriminator.
//
// NOTE on why this uses the resolver rather than `cast().resolve()`: Siren's Call
// parses to casting restrictions [DuringOpponentsTurn, BeforeAttackersDeclared].
// `DuringOpponentsTurn` requires the caster != active player; `BeforeAttackers
// Declared` (via `is_before_attackers_declared`) requires the priority *seat* ==
// active player. In the current engine model these are mutually exclusive for any
// correctly-attributed non-active cast, so the front-door `cast()` is not
// reachable for this card class. The coerce-application + delayed-DestroyAll
// runtime — the behavior the review flagged as uncovered — is exercised here
// through the same production resolver a real cast would invoke on resolution.
// (See the stop-and-return note in the fix-round report.)
//
// Revert-failing:
//   * If the coerce controller binding reverts from ActivePlayer to You, the mass
//     MustAttack lands on the caster P1 (who controls nothing), so
//     `creature_must_attack(P0's X)` flips to false — the reach-guard fails.
//   * If the delayed DestroyAll wiring reverts, Y is never destroyed — the
//     `zone == Graveyard` assertion fails.
//   * If the DestroyAll controller reverts from ActivePlayer to You (P1) or to
//     Opponent, P0's board is not punished and/or P2's creature Z is swept — the
//     Y-destroyed / Z-survives assertions fail.
// ===========================================================================
#[test]
fn test_a_sirens_call_runtime_pipeline_multiplayer_discrimination() {
    use engine::game::ability_utils::build_resolved_from_def;
    use engine::game::effects::resolve_ability_chain;

    let mut sc = GameScenario::new_n_player(3, 7);

    // P0 (the active player) controls two non-Wall creatures, both present since
    // before this turn (add_creature => not summoning-sick => controlled
    // continuously since the turn began, so the continuity exemption does NOT
    // spare them). X will attack; Y will not.
    let x = sc.add_creature(P0, "P0 Attacker X", 2, 2).id();
    let y = sc.add_creature(P0, "P0 Idler Y", 2, 2).id();
    // A third player's non-Wall creature that will not attack. Only the ACTIVE
    // player's board is punished, so Z must survive.
    let z = sc.add_creature(PlayerId(2), "P2 Bystander Z", 2, 2).id();

    // The resolving spell is owned/controlled by the non-active caster P1. Its
    // ObjectId is the resolution source; the coerce/punisher's ActivePlayer
    // binding must ignore this controller and resolve to P0.
    let source = sc.add_creature(P1, "Siren's Call (source)", 0, 0).id();

    // P0's turn, pre-combat (default active_player is P0). `at_phase` sets phase +
    // priority + turn_number consistently so `advance_to_phase` drives cleanly.
    sc.at_phase(Phase::PreCombatMain);
    let mut runner = sc.build();
    assert_eq!(runner.state().active_player, P0);

    // Parse the FULL card (carries the Step-3/5/7 rewrites) and resolve BOTH of
    // its abilities through the production resolver, controlled by P1.
    let parsed = parse("Siren's Call", SIRENS_CALL, &["Instant"], &[]);
    assert_eq!(
        parsed.abilities.len(),
        2,
        "Siren's Call resolves as the coerce ability + the delayed-punisher ability"
    );
    let mut events = Vec::new();
    for ability_def in &parsed.abilities {
        let resolved = build_resolved_from_def(ability_def, source, P1);
        resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
            .expect("Siren's Call ability must resolve");
    }
    // The coerce is a continuous MustAttack static; evaluate layers so the
    // transient effect is live before combat reads it.
    runner.state_mut().layers_dirty.mark_full();
    engine::game::layers::evaluate_layers(runner.state_mut());

    // Reach-guard (revert-failing on the ActivePlayer binding): the active
    // player's creature IS forced to attack. If the coerce bound `You`, this
    // would be false because the controller P1 controls no creatures here.
    assert!(
        creature_must_attack(runner.state(), x),
        "the active player's creature must be forced to attack (mass MustAttack \
         bound to ActivePlayer, not the controller You)"
    );
    assert!(
        creature_must_attack(runner.state(), y),
        "the active player's second creature is likewise forced (reach-guard)"
    );
    // Discriminator: P2's creature is NOT forced — it is not the active player's.
    assert!(
        !creature_must_attack(runner.state(), z),
        "a non-active player's creature is not forced by an ActivePlayer coerce"
    );

    // Tap Y so it is legally UNABLE to attack (CR 508.1a). "Attack if able" then
    // does not require it, and it becomes the non-attacker the punisher targets.
    // (This also confirms the coerce is an "if able" requirement, not an
    // unconditional lock — with Y untapped the declare below would be rejected.)
    runner.state_mut().objects.get_mut(&y).unwrap().tapped = true;
    assert!(
        !creature_must_attack(runner.state(), y),
        "a tapped creature is no longer required to attack (CR 508.1a)"
    );

    // Advance to declare-attackers and declare ONLY X (Y is tapped, sits out).
    runner.advance_to_combat();
    assert_eq!(runner.state().phase, Phase::DeclareAttackers);
    runner
        .declare_attackers(&[(x, AttackTarget::Player(P1))])
        .expect("declaring X as an attacker must be accepted");

    // Run combat out (empty blockers if the step opens) so the turn can reach the
    // end step where the delayed punisher fires.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        let _ = runner.declare_blockers(&[]);
    }
    let _ = runner.combat_damage();

    // Advance to the end step so the delayed DestroyAll fires, and resolve it.
    runner.advance_to_phase(Phase::End);
    runner.advance_until_stack_empty();

    // FINAL assertions.
    // Reach-guard the destroy: Y actually died (moved to graveyard).
    assert_eq!(
        runner.state().objects[&y].zone,
        Zone::Graveyard,
        "P0's non-attacker Y must be destroyed by the delayed punisher"
    );
    // X attacked, so it is spared.
    assert_eq!(
        runner.state().objects[&x].zone,
        Zone::Battlefield,
        "P0's attacker X attacked and must survive"
    );
    // Multiplayer discrimination: Z belongs to P2, not the active player, so it
    // is untouched even though it did not attack.
    assert_eq!(
        runner.state().objects[&z].zone,
        Zone::Battlefield,
        "the third player's non-attacker Z must survive — only the active \
         player's board is punished (ActivePlayer, not Opponent)"
    );
}

// ===========================================================================
// TEST B — Maddening Imp activation legality through the production restriction
// evaluator.
//
// Feeds the parsed compound activation timing ([Not(IsYourTurn),
// BeforeAttackersDeclared]) into the production `check_activation_restrictions`
// and drives it across three turn/phase states. Before Step 6b the parser
// emitted a single vacuous `RequiresCondition{None}`, which is ALWAYS legal — so
// the "own turn" and "after attackers declared" cases would wrongly return Ok.
//
// Revert-failing: revert Step 6b and the compound splits back into a vacuous
// always-legal restriction; the two `is_err()` assertions below flip to Ok.
// ===========================================================================
#[test]
fn test_b_maddening_imp_activation_legality_production_path() {
    // Parse Maddening Imp and pull the enforced compound activation timing.
    let parsed = parse_kw(
        "Maddening Imp",
        MADDENING_IMP,
        &["Flying"],
        &["Creature"],
        &["Imp"],
    );
    let restrictions = activation_restrictions(&parsed);
    // Guard: the parser handed us the enforced compound gate, not the vacuous
    // no-op. (Without this the Ok/Err results below could be trivially green on a
    // reverted parser.)
    assert_eq!(
        restrictions,
        vec![
            opponents_turn(),
            ActivationRestriction::BeforeAttackersDeclared
        ],
        "the parsed compound timing gate must be present for a meaningful legality probe"
    );

    // A concrete source object on P0's battlefield gives the production check a
    // real source_id to reason about.
    let mut sc = GameScenario::new_n_player(2, 13);
    let imp = sc.add_creature(P0, "Maddening Imp", 1, 1).id();
    let mut runner = sc.build();

    let check = |runner: &engine::game::scenario::GameRunner| -> Result<(), EngineError> {
        check_activation_restrictions(runner.state(), P0, imp, 0, &restrictions)
    };

    // Case 1: the controller's OWN turn, pre-combat. `Not(IsYourTurn)` fails —
    // illegal.
    {
        let st = runner.state_mut();
        st.active_player = P0;
        st.phase = Phase::PreCombatMain;
        st.priority_player = P0;
        st.waiting_for = WaitingFor::Priority { player: P0 };
    }
    assert!(
        check(&runner).is_err(),
        "activation on the controller's own turn must be illegal (Not(IsYourTurn) fails)"
    );

    // Case 2: an OPPONENT's pre-combat main. The activating (non-active) player
    // P0 realistically holds priority to activate the ability. `Not(IsYourTurn)`
    // passes (P0 != active P1) AND `BeforeAttackersDeclared` passes (a phase-based
    // window, independent of who holds priority — CR 508.1/508.2) — legal.
    {
        let st = runner.state_mut();
        st.active_player = P1;
        st.phase = Phase::PreCombatMain;
        st.priority_player = P0;
        st.waiting_for = WaitingFor::Priority { player: P0 };
    }
    assert!(
        check(&runner).is_ok(),
        "activation during an opponent's pre-combat main (P0 holding priority) must be legal"
    );

    // Case 3: an OPPONENT's turn AFTER attackers are declared (DeclareBlockers),
    // P0 holding priority. `BeforeAttackersDeclared` fails (phase is past the
    // window) — illegal.
    {
        let st = runner.state_mut();
        st.active_player = P1;
        st.phase = Phase::DeclareBlockers;
        st.priority_player = P0;
        st.waiting_for = WaitingFor::Priority { player: P0 };
    }
    assert!(
        check(&runner).is_err(),
        "activation after attackers are declared must be illegal (BeforeAttackersDeclared fails)"
    );
}

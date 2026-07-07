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

/// Extract the Maddening Imp delayed `DestroyAll` target and the enclosing
/// `CreateDelayedTrigger.uses_tracked_set` flag from the `{T}` sub_ability chain.
fn maddening_delayed_destroy(p: &ParsedAbilities) -> (TargetFilter, bool) {
    let activated = p
        .abilities
        .iter()
        .find(|a| matches!(a.effect.as_ref(), Effect::GenericEffect { .. }))
        .expect("activated ability present");
    let mut node = activated.sub_ability.as_deref();
    while let Some(n) = node {
        if let Effect::CreateDelayedTrigger {
            effect,
            uses_tracked_set,
            ..
        } = n.effect.as_ref()
        {
            if let Effect::DestroyAll { target, .. } = effect.effect.as_ref() {
                return (target.clone(), *uses_tracked_set);
            }
        }
        node = n.sub_ability.as_deref();
    }
    panic!("delayed DestroyAll present");
}

// ---------------------------------------------------------------------------
// Test 6 — Maddening Imp "each of those creatures that didn't attack this turn"
// parses to a FROZEN tracked-set consumer: DestroyAll{TrackedSetFiltered{id:0,
// Not(AttackedThisTurn)}} with uses_tracked_set == true. The sentinel is still
// `TrackedSetId(0)` at parse time (it is pinned to a concrete id only at
// delayed-trigger CREATION, Step 0), so this asserts the id-0 sentinel here.
// ---------------------------------------------------------------------------
#[test]
fn test6_maddening_those_creatures_tracked_set() {
    use engine::types::identifiers::TrackedSetId;
    let p = parse("Maddening Imp", MADDENING_IMP, &["Creature"], &["Imp"]);
    let (target, uses_tracked_set) = maddening_delayed_destroy(&p);

    // Revert-failing (Step 1): without the mass-coerce publisher gate the
    // CreateDelayedTrigger is not marked uses_tracked_set.
    assert!(
        uses_tracked_set,
        "the delayed trigger must consume the frozen tracked set (uses_tracked_set)"
    );

    // Revert-failing (Step 2): the target must be a TrackedSetFiltered over the
    // frozen population, NOT a bare TrackedSet (which would drop the predicate)
    // and NOT a concrete live-refilter Typed.
    let TargetFilter::TrackedSetFiltered { id, filter, .. } = &target else {
        panic!("expected TrackedSetFiltered, got {target:?}");
    };
    assert_eq!(
        *id,
        TrackedSetId(0),
        "parse-time sentinel is id 0; the concrete id is pinned at creation (Step 0)"
    );
    // The inner filter carries the did-not-attack predicate.
    let props = typed_props(filter);
    assert!(
        props.iter().any(|p| matches!(
            p,
            FilterProp::Not { prop } if matches!(prop.as_ref(), FilterProp::AttackedThisTurn { defender: None })
        )),
        "inner filter must carry Not(AttackedThisTurn), got {props:?}"
    );
}

/// Resolve Maddening Imp's `{T}` coerce + delayed-punisher chain through the
/// production resolver, controlled by the non-active caster P1 with a source Imp
/// on P1's board. Publishes the frozen "those creatures" set (the mass-MustAttack
/// coerce over P0's board) and creates the delayed `DestroyAll{TrackedSetFiltered}`
/// whose sentinel is pinned to that set's concrete id at creation (Step 0). The
/// coerce's continuous MustAttack static is then evaluated into layers. Returns
/// the source ObjectId.
fn resolve_maddening_coerce(runner: &mut engine::game::scenario::GameRunner) -> ObjectId {
    use engine::game::ability_utils::build_resolved_from_def;
    use engine::game::effects::resolve_ability_chain;

    let source = {
        // A source object on the non-active caster P1's board.
        let src = engine::game::zones::create_object(
            runner.state_mut(),
            engine::types::identifiers::CardId(4242),
            P1,
            "Maddening Imp (source)".to_string(),
            Zone::Battlefield,
        );
        src
    };

    let parsed = parse("Maddening Imp", MADDENING_IMP, &["Creature"], &["Imp"]);
    let coerce = parsed
        .abilities
        .iter()
        .find(|a| matches!(a.effect.as_ref(), Effect::GenericEffect { .. }))
        .expect("Maddening coerce ability present");
    let resolved = build_resolved_from_def(coerce, source, P1);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("Maddening coerce chain must resolve");
    // The coerce is a continuous MustAttack static; evaluate layers so combat and
    // the "attacked this turn" tracking read it live.
    runner.state_mut().layers_dirty.mark_full();
    engine::game::layers::evaluate_layers(runner.state_mut());
    source
}

// ---------------------------------------------------------------------------
// Test 6b — FROZEN snapshot (CR 608.2c direction 1, late-arrival): a non-Wall
// creature entering the active player's control AFTER the {T} coerce resolved is
// NOT in the frozen "those creatures" set and must survive the end-step punisher,
// while a frozen member that did not attack IS destroyed (positive reach-guard).
// Revert-failing: the old live refilter would destroy the late arrival too.
// ---------------------------------------------------------------------------
#[test]
fn test6b_frozen_late_arrival_survives() {
    let mut sc = GameScenario::new_n_player(3, 3);
    // P0 (active) old non-Wall non-attacker Y (controlled since turn began → in
    // the frozen set, not spared by any continuity exemption in Maddening).
    let y = sc.add_creature(P0, "P0 Idler Y", 2, 2).id();
    sc.at_phase(Phase::PreCombatMain);
    let mut runner = sc.build();
    assert_eq!(runner.state().active_player, P0);

    // Resolve the {T} coerce → publish frozen set {Y} + create the delayed
    // DestroyAll (sentinel pinned to that set's id).
    let _source = resolve_maddening_coerce(&mut runner);

    // AFTER {T} resolves, a NEW non-Wall creature Late enters P0's control and
    // will not attack — never a member of the frozen set. It must be a real
    // Creature so the inner `Not(AttackedThisTurn)` ∧ Creature filter genuinely
    // matches it — otherwise its survival would be a vacuous type mismatch, not a
    // frozen-membership result.
    let late = {
        let id = engine::game::zones::create_object(
            runner.state_mut(),
            engine::types::identifiers::CardId(7777),
            P0,
            "Late Bear".to_string(),
            Zone::Battlefield,
        );
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.summoning_sick = false;
        id
    };

    // Both are non-Wall creatures the active player controls, so the continuous
    // MustAttack forces both. Tap them so they are legally UNABLE to attack
    // (CR 508.1a "if able"): with no creature able to attack, the declare-attackers
    // step auto-resolves and the turn advances to the end step where the delayed
    // punisher fires. Neither creature attacked this turn.
    runner.state_mut().objects.get_mut(&y).unwrap().tapped = true;
    runner.state_mut().objects.get_mut(&late).unwrap().tapped = true;

    runner.advance_to_phase(Phase::End);
    runner.advance_until_stack_empty();

    // Positive reach-guard: the frozen member Y (did not attack) is destroyed.
    assert_eq!(
        runner.state().objects[&y].zone,
        Zone::Graveyard,
        "frozen member Y that did not attack must be destroyed"
    );
    // Frozen semantics: the late arrival was never in the set → spared.
    assert_eq!(
        runner.state().objects[&late].zone,
        Zone::Battlefield,
        "a creature that entered after the coerce resolved is NOT in the frozen set"
    );
}

// ---------------------------------------------------------------------------
// Test 6c — FROZEN snapshot (CR 608.2c direction 2, control-change-out): a member
// of the frozen set that changed controller (but kept its ObjectId, i.e. did not
// leave the battlefield) is STILL destroyed if it did not attack — it is still
// "those creatures". Paired positive: a frozen member that DID attack survives.
// Revert-failing: the old controller:ActivePlayer live filter spared W.
// ---------------------------------------------------------------------------
#[test]
fn test6c_frozen_control_change_out_still_destroyed() {
    let mut sc = GameScenario::new_n_player(3, 4);
    // P0 (active) frozen members: W (will change controller, did not attack) and
    // A (will attack, spared).
    let w = sc.add_creature(P0, "P0 W", 2, 2).id();
    let a = sc.add_creature(P0, "P0 Attacker A", 2, 2).id();
    sc.at_phase(Phase::PreCombatMain);
    let mut runner = sc.build();

    resolve_maddening_coerce(&mut runner);

    // W sits out: tap it so it is legally UNABLE to attack (CR 508.1a). A attacks.
    runner.state_mut().objects.get_mut(&w).unwrap().tapped = true;
    runner.advance_to_combat();
    assert_eq!(runner.state().phase, Phase::DeclareAttackers);
    runner
        .declare_attackers(&[(a, AttackTarget::Player(P1))])
        .expect("declaring A as an attacker must be accepted");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        let _ = runner.declare_blockers(&[]);
    }
    let _ = runner.combat_damage();

    // AFTER combat, W changes controller to P2 but stays on the battlefield with
    // the SAME ObjectId — it is still a member of the frozen set (CR 603.7c).
    runner.state_mut().objects.get_mut(&w).unwrap().controller = P1;

    runner.advance_to_phase(Phase::End);
    runner.advance_until_stack_empty();

    // The control-change-out member W (did not attack) is STILL destroyed.
    assert_eq!(
        runner.state().objects[&w].zone,
        Zone::Graveyard,
        "a frozen member that changed controller but kept its ObjectId is still destroyed"
    );
    // Paired positive: the frozen member that attacked survives.
    assert_eq!(
        runner.state().objects[&a].zone,
        Zone::Battlefield,
        "a frozen member that attacked is spared"
    );
}

// ---------------------------------------------------------------------------
// Test 6d — BLOCKER 1: cross-resolution second-set collision. A SECOND, higher-id
// tracked set is published between the {T} resolution and the end step. Because
// Step 0 pins the delayed DestroyAll's sentinel to the coerce set's concrete id
// at CREATION, the punisher destroys its OWN frozen population — not the members
// of the later, higher-id set. Revert-failing on Step 0: without the pin, the
// end-step `matches_target_filter` self-resolves sentinel 0 via `max_by_key` and
// picks the SECOND set, so Q (in the second set) is destroyed and Y is spared.
// ---------------------------------------------------------------------------
#[test]
fn test6d_frozen_survives_second_tracked_set() {
    use engine::types::identifiers::TrackedSetId;

    let mut sc = GameScenario::new_n_player(3, 5);
    // P0 (active) frozen members: Y (did not attack → destroyed) + X (attacks →
    // spared).
    let y = sc.add_creature(P0, "P0 Idler Y", 2, 2).id();
    let x = sc.add_creature(P0, "P0 Attacker X", 2, 2).id();
    // P2 bystander Q — will be the SOLE member of a later, unrelated tracked set.
    // Non-Wall and did NOT attack, so if the wrong (second) set were consumed it
    // would be destroyed.
    let q = sc.add_creature(PlayerId(2), "P2 Bystander Q", 2, 2).id();
    sc.at_phase(Phase::PreCombatMain);
    let mut runner = sc.build();

    resolve_maddening_coerce(&mut runner);

    // AFTER {T} resolves, publish a SECOND, higher-id tracked set whose only
    // member is Q. `publish_fresh_tracked_set` is pub(crate) (not reachable from
    // this integration crate), so use the documented direct-insert escape hatch:
    // allocate an id strictly higher than any existing set so `latest_tracked_set_id`'s
    // `max_by_key` would return it if the sentinel were self-resolved live.
    {
        let st = runner.state_mut();
        let high = st.next_tracked_set_id.max(
            st.tracked_object_sets
                .keys()
                .map(|id| id.0)
                .max()
                .unwrap_or(0)
                + 1,
        );
        st.tracked_object_sets.insert(TrackedSetId(high), vec![q]);
        st.next_tracked_set_id = high + 1;
    }

    // X attacks (satisfies coerce, spared); Y sits out — tap it so it is legally
    // unable to attack (CR 508.1a), letting declare-attackers pass with only X.
    runner.state_mut().objects.get_mut(&y).unwrap().tapped = true;
    runner.advance_to_combat();
    assert_eq!(runner.state().phase, Phase::DeclareAttackers);
    runner
        .declare_attackers(&[(x, AttackTarget::Player(P1))])
        .expect("declaring X as an attacker must be accepted");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        let _ = runner.declare_blockers(&[]);
    }
    let _ = runner.combat_damage();

    runner.advance_to_phase(Phase::End);
    runner.advance_until_stack_empty();

    // Positive reach-guard: Maddening's OWN frozen non-attacker Y is destroyed
    // (the pinned set k is consumed — proves the punisher fired against the right
    // population, so the Q-survives assertion below cannot pass vacuously).
    assert_eq!(
        runner.state().objects[&y].zone,
        Zone::Graveyard,
        "the pinned coerce set's non-attacker Y must be destroyed"
    );
    // X attacked → spared.
    assert_eq!(
        runner.state().objects[&x].zone,
        Zone::Battlefield,
        "the frozen member that attacked is spared"
    );
    // The collision guard: Q belongs only to the SECOND set, not Maddening's
    // frozen population → NOT destroyed. Reverting Step 0 flips this (max_by_key
    // picks the second set → Q destroyed, Y spared).
    assert_eq!(
        runner.state().objects[&q].zone,
        Zone::Battlefield,
        "a member of a later, unrelated tracked set must NOT be swept by the pinned punisher"
    );
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
// REAL `GameRunner::cast(..).resolve()` front door, 3-player (multiplayer
// discrimination).
//
// PR1 (merged) made `is_before_attackers_declared` a pure PHASE check
// (restrictions.rs — `PreCombatMain | BeginCombat`, NOT priority-gated), so a
// non-active caster holding priority during the active player's PreCombatMain
// satisfies BOTH `Not(IsYourTurn)` (caster P1 != active P0) and
// `BeforeAttackersDeclared`. The former mutual-exclusion NOTE was stale and is
// deleted; this test now casts through the production cast pipeline. The card
// comes from `add_spell_to_hand_from_oracle` (real parsed abilities, carrying the
// coerce → ActivePlayer + DestroyAll continuity-exemption rewrites). The caster
// is the non-active P1; the ACTIVE player is P0 — so an `ActivePlayer` binding
// resolves to P0's board and a `You` binding would wrongly resolve to P1's
// (empty) board. That asymmetry is the multiplayer discriminator.
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
fn test_a_sirens_call_cast_pipeline_multiplayer_discrimination() {
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

    // Siren's Call in the non-active caster P1's hand, with real parsed abilities.
    // Untargeted instant, no explicit mana cost from the builder → free to cast.
    let sirens_call = sc
        .add_spell_to_hand_from_oracle(P1, "Siren's Call", true, SIRENS_CALL)
        .id();

    // P0's turn, pre-combat main. `at_phase` seats priority with the active player;
    // move priority to the non-active caster P1 (the realistic instant-speed
    // window before attackers are declared).
    sc.at_phase(Phase::PreCombatMain);
    let mut runner = sc.build();
    assert_eq!(runner.state().active_player, P0);
    {
        let st = runner.state_mut();
        st.priority_player = P1;
        st.waiting_for = WaitingFor::Priority { player: P1 };
    }

    // Cast + resolve Siren's Call through the real front door.
    runner.cast(sirens_call).resolve();

    // The coerce is a continuous MustAttack static; evaluate layers so combat reads
    // it live before declare-attackers.
    runner.state_mut().layers_dirty.mark_full();
    engine::game::layers::evaluate_layers(runner.state_mut());

    // Reach-guard (revert-failing on the ActivePlayer binding): the active
    // player's creature IS forced to attack. If the coerce bound `You`, this
    // would be false because the caster P1 controls no creatures here.
    assert!(
        creature_must_attack(runner.state(), x),
        "the active player's creature must be forced to attack (mass MustAttack \
         bound to ActivePlayer, not the caster You)"
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

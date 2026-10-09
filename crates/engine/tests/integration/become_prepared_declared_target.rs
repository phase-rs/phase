//! "Target <filter> becomes (un)prepared" declares a real target.
//!
//! CR 115.1c / CR 115.1d: an activated or triggered ability that says
//! "target [something]" is targeted, and its target is chosen as the ability
//! is put on the stack (CR 601.2c, via CR 602.2b for activated abilities and
//! CR 603.3d for triggered abilities). CR 722.3a: only a permanent with a
//! prepare spell can become prepared ("Only creatures with prepare spells can
//! become prepared."). CR 722.3b: becoming unprepared removes the designation.
//!
//! Cards (verbatim Oracle text, checked against Scryfall):
//! - Skycoach Waypoint: `{3}, {T}: Target creature becomes prepared.`
//! - Hexhaven Dueling Arena: `{2}, {T}: Target creature that attacked this
//!   turn becomes prepared. Activate only as a sorcery.` and `{4}, {T}: Target
//!   creature becomes prepared.`
//! - Biblioplex Tomekeeper: ETB "choose up to one" with a prepare mode and an
//!   unprepare mode, each naming "target creature".
//!
//! Regression mechanism: the parser bound a declared-target subject to
//! `TargetFilter::ParentTarget`, a context ref, so no target slot was built and
//! Hexhaven's "that attacked this turn" restriction was dropped. At resolution
//! `ParentTarget` with no targets fell back to the ability's own source.
//!
//! Revert-fail: with the parser arm reverted, no target prompt appears
//! (`announce_activation` / `drive_tomekeeper_etb` prompt assertions fail) and
//! the chosen creature's `prepared` state never changes.

use engine::game::effects::prepare::prepare_object;
use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{
    AbilityDefinition, ActivationRestriction, Effect, EffectKind, FilterProp, TargetFilter,
    TargetRef, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

const SKYCOACH_WAYPOINT: &str = "{T}: Add {C}.\n{3}, {T}: Target creature becomes prepared. (Only creatures with prepare spells can become prepared.)";

const HEXHAVEN_DUELING_ARENA: &str = "{T}: Add {C}.\n{2}, {T}: Target creature that attacked this turn becomes prepared. Activate only as a sorcery. (Only creatures with prepare spells can become prepared.)\n{4}, {T}: Target creature becomes prepared.";

const BIBLIOPLEX_TOMEKEEPER: &str = "When this creature enters, choose up to one —\n• Target creature becomes prepared. (Only creatures with prepare spells can become prepared.)\n• Target creature becomes unprepared.";

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Give `id` the prepare-spell face that makes it eligible to become prepared
/// (CR 722.3a). Mirrors `fra_bloodline_recollector.rs`.
fn give_prepare_face(runner: &mut GameRunner, id: ObjectId) {
    runner.state_mut().objects.get_mut(&id).unwrap().back_face = Some(BackFaceData {
        layout_kind: Some(LayoutKind::Prepare),
        ..BackFaceData::default()
    });
}

fn is_prepared(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].prepared.is_some()
}

/// Pre-prepare `id` through the production authority (never by writing the
/// field), and assert the precondition took.
fn pre_prepare(runner: &mut GameRunner, id: ObjectId) {
    let mut events = Vec::new();
    prepare_object(runner.state_mut(), id, &mut events);
    assert!(
        is_prepared(runner, id),
        "precondition: prepare_object must prepare an eligible creature"
    );
}

fn became_prepared(events: &[GameEvent], id: ObjectId) -> bool {
    events
        .iter()
        .any(|e| matches!(e, GameEvent::BecamePrepared { object_id } if *object_id == id))
}

fn became_unprepared(events: &[GameEvent], id: ObjectId) -> bool {
    events
        .iter()
        .any(|e| matches!(e, GameEvent::BecameUnprepared { object_id } if *object_id == id))
}

fn any_became_prepared(events: &[GameEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, GameEvent::BecamePrepared { .. }))
}

fn effect_resolved(events: &[GameEvent], kind: EffectKind) -> bool {
    events
        .iter()
        .any(|e| matches!(e, GameEvent::EffectResolved { kind: k, .. } if *k == kind))
}

fn colorless(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

/// Index of the `nth` (0-based) activated ability on `source` whose effect is
/// `Effect::BecomePrepared` — never a hard-coded position.
fn nth_prepare_ability(runner: &GameRunner, source: ObjectId, nth: usize) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .enumerate()
        .filter(|(_, a)| matches!(a.effect.as_ref(), Effect::BecomePrepared { .. }))
        .nth(nth)
        .map(|(i, _)| i)
        .unwrap_or_else(|| panic!("source must have BecomePrepared ability #{nth}"))
}

/// The legal targets of the current target-selection slot, or `None` when the
/// engine is not prompting for a target.
fn current_target_prompt(runner: &GameRunner) -> Option<Vec<TargetRef>> {
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        } => Some(target_slots[selection.current_slot].legal_targets.clone()),
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } => Some(target_slots[selection.current_slot].legal_targets.clone()),
        _ => None,
    }
}

/// CR 602.2a + CR 601.2c: announce the activation and return the legal
/// targets of the declared target slot. Fails if no target prompt appears —
/// the declared-target slot is exactly what this file proves exists.
fn announce_activation(
    runner: &mut GameRunner,
    source: ObjectId,
    ability_index: usize,
    events: &mut Vec<GameEvent>,
) -> Vec<TargetRef> {
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
        })
        .expect("ActivateAbility must be accepted");
    events.extend(result.events);
    current_target_prompt(runner).unwrap_or_else(|| {
        panic!(
            "CR 115.1c + CR 601.2c: 'target creature' must declare a target slot; \
             engine is waiting for {:?}",
            runner.waiting_for_kind()
        )
    })
}

/// Choose `target`, pay the mana cost from the pool and pass priority until
/// the stack is empty, accumulating every emitted event.
fn choose_and_resolve(runner: &mut GameRunner, target: ObjectId, events: &mut Vec<GameEvent>) {
    let result = runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("choosing a legal target must be accepted");
    events.extend(result.events);
    for _ in 0..64 {
        match runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing mana payment from the pool must be accepted");
                events.extend(result.events);
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must be accepted");
                events.extend(result.events);
            }
            WaitingFor::Priority { .. } => return,
            _ => panic!(
                "unexpected WaitingFor while resolving the ability: {}",
                runner.waiting_for_kind()
            ),
        }
    }
    panic!("the ability did not resolve within the step budget");
}

/// Battlefield fixture for the land tests. `eligible`/`bystander`/`opponent`
/// have a prepare face; `ineligible` has none.
struct LandFixture {
    runner: GameRunner,
    land: ObjectId,
    eligible: ObjectId,
    bystander: ObjectId,
    opponent: ObjectId,
    ineligible: ObjectId,
}

fn land_fixture(name: &str, oracle: &str, mana: usize) -> LandFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_land_from_oracle(P0, name, oracle).id();
    let eligible = scenario.add_creature(P0, "Eligible Scholar", 2, 2).id();
    let bystander = scenario.add_creature(P0, "Bystander Scholar", 2, 2).id();
    let opponent = scenario.add_creature(P1, "Rival Scholar", 2, 2).id();
    let ineligible = scenario.add_creature(P0, "Plain Bear", 2, 2).id();
    scenario.with_mana_pool(P0, colorless(mana));
    let mut runner = scenario.build();
    for id in [eligible, bystander, opponent] {
        give_prepare_face(&mut runner, id);
    }
    for id in [eligible, bystander, opponent, ineligible] {
        assert!(
            !is_prepared(&runner, id),
            "precondition: nothing is prepared"
        );
    }
    LandFixture {
        runner,
        land,
        eligible,
        bystander,
        opponent,
        ineligible,
    }
}

// ---------------------------------------------------------------------------
// SHAPE: parse output (not revert-fail on its own; the runtime tests are)
// ---------------------------------------------------------------------------

fn collect_prepare_targets(def: &AbilityDefinition, out: &mut Vec<(EffectKind, TargetFilter)>) {
    match def.effect.as_ref() {
        Effect::BecomePrepared { target } => out.push((EffectKind::BecomePrepared, target.clone())),
        Effect::BecomeUnprepared { target } => {
            out.push((EffectKind::BecomeUnprepared, target.clone()))
        }
        _ => {}
    }
    for nested in def
        .sub_ability
        .iter()
        .chain(def.else_ability.iter())
        .map(|b| b.as_ref())
        .chain(def.mode_abilities.iter())
    {
        collect_prepare_targets(nested, out);
    }
}

fn parse_card(oracle: &str, name: &str, types: &[&str]) -> ParsedAbilities {
    let types: Vec<String> = types.iter().map(|t| t.to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &[]);
    assert!(
        !serde_json::to_string(&parsed)
            .expect("serialize parsed abilities")
            .contains("\"Unimplemented\""),
        "verbatim {name} Oracle must contain no Unimplemented node"
    );
    parsed
}

fn assert_declared_creature_target(target: &TargetFilter) -> &[FilterProp] {
    let TargetFilter::Typed(typed) = target else {
        panic!("declared target must keep its typed filter, got {target:?}");
    };
    assert!(
        typed.type_filters.contains(&TypeFilter::Creature),
        "declared target must be a creature filter, got {typed:?}"
    );
    &typed.properties
}

/// SHAPE: every declared "target creature ... becomes (un)prepared" keeps its
/// typed creature filter (not `ParentTarget`), and Hexhaven's first ability
/// keeps its "that attacked this turn" restriction plus its sorcery timing.
#[test]
fn declared_target_subjects_keep_their_typed_filter() {
    let skycoach = parse_card(SKYCOACH_WAYPOINT, "Skycoach Waypoint", &["Land"]);
    let mut found = Vec::new();
    skycoach
        .abilities
        .iter()
        .for_each(|a| collect_prepare_targets(a, &mut found));
    assert_eq!(found.len(), 1, "Skycoach has one prepare ability");
    assert!(assert_declared_creature_target(&found[0].1).is_empty());

    let hexhaven = parse_card(HEXHAVEN_DUELING_ARENA, "Hexhaven Dueling Arena", &["Land"]);
    let prepare_abilities: Vec<&AbilityDefinition> = hexhaven
        .abilities
        .iter()
        .filter(|a| matches!(a.effect.as_ref(), Effect::BecomePrepared { .. }))
        .collect();
    assert_eq!(
        prepare_abilities.len(),
        2,
        "Hexhaven has two prepare abilities"
    );
    let Effect::BecomePrepared { target: first } = prepare_abilities[0].effect.as_ref() else {
        unreachable!()
    };
    assert_eq!(
        assert_declared_creature_target(first),
        &[FilterProp::AttackedThisTurn { defender: None }],
        "Hexhaven's {{2}} ability must keep 'that attacked this turn'"
    );
    assert!(prepare_abilities[0]
        .activation_restrictions
        .contains(&ActivationRestriction::AsSorcery));
    let Effect::BecomePrepared { target: second } = prepare_abilities[1].effect.as_ref() else {
        unreachable!()
    };
    assert!(assert_declared_creature_target(second).is_empty());
    assert!(prepare_abilities[1].activation_restrictions.is_empty());

    let tomekeeper = parse_card(
        BIBLIOPLEX_TOMEKEEPER,
        "Biblioplex Tomekeeper",
        &["Artifact", "Creature"],
    );
    let mut found = Vec::new();
    for trigger in &tomekeeper.triggers {
        if let Some(execute) = trigger.execute.as_deref() {
            collect_prepare_targets(execute, &mut found);
        }
    }
    let kinds: Vec<EffectKind> = found.iter().map(|(k, _)| *k).collect();
    assert_eq!(
        kinds,
        vec![EffectKind::BecomePrepared, EffectKind::BecomeUnprepared],
        "Tomekeeper has a prepare mode and an unprepare mode"
    );
    for (_, target) in &found {
        assert!(assert_declared_creature_target(target).is_empty());
    }
}

// ---------------------------------------------------------------------------
// Skycoach Waypoint
// ---------------------------------------------------------------------------

/// CR 602.2b + CR 601.2c + CR 722.3a: the chosen creature becomes prepared;
/// no other creature does.
///
/// DISCRIMINATION: reverted, no target prompt appears (`announce_activation`
/// panics) and `eligible` stays unprepared.
#[test]
fn skycoach_waypoint_prepares_the_chosen_creature() {
    let LandFixture {
        mut runner,
        land,
        eligible,
        bystander,
        opponent,
        ineligible,
    } = land_fixture("Skycoach Waypoint", SKYCOACH_WAYPOINT, 3);
    let idx = nth_prepare_ability(&runner, land, 0);
    let mut events = Vec::new();

    let legal = announce_activation(&mut runner, land, idx, &mut events);
    for id in [eligible, bystander, opponent, ineligible] {
        assert!(
            legal.contains(&TargetRef::Object(id)),
            "every creature is a legal 'target creature'"
        );
    }
    choose_and_resolve(&mut runner, eligible, &mut events);

    assert!(
        is_prepared(&runner, eligible),
        "the chosen creature must become prepared"
    );
    assert!(became_prepared(&events, eligible));
    for id in [bystander, opponent, ineligible] {
        assert!(
            !is_prepared(&runner, id),
            "an unchosen creature stays unprepared"
        );
    }
    assert!(runner.state().stack.is_empty());
}

/// CR 115.1c: "target creature" includes a creature another player controls.
#[test]
fn skycoach_waypoint_prepares_an_opponents_creature() {
    let LandFixture {
        mut runner,
        land,
        eligible,
        opponent,
        ..
    } = land_fixture("Skycoach Waypoint", SKYCOACH_WAYPOINT, 3);
    let idx = nth_prepare_ability(&runner, land, 0);
    let mut events = Vec::new();

    let legal = announce_activation(&mut runner, land, idx, &mut events);
    assert!(legal.contains(&TargetRef::Object(opponent)));
    choose_and_resolve(&mut runner, opponent, &mut events);

    assert!(
        is_prepared(&runner, opponent),
        "the opponent's chosen creature must become prepared"
    );
    assert!(became_prepared(&events, opponent));
    assert!(!is_prepared(&runner, eligible));
}

/// CR 722.3a: a legal target with no prepare spell does not become prepared.
/// Reach-guards: the target prompt accepted it and the effect resolved.
#[test]
fn skycoach_waypoint_ineligible_target_stays_unprepared() {
    let LandFixture {
        mut runner,
        land,
        ineligible,
        ..
    } = land_fixture("Skycoach Waypoint", SKYCOACH_WAYPOINT, 3);
    let idx = nth_prepare_ability(&runner, land, 0);
    let mut events = Vec::new();

    let legal = announce_activation(&mut runner, land, idx, &mut events);
    assert!(legal.contains(&TargetRef::Object(ineligible)));
    choose_and_resolve(&mut runner, ineligible, &mut events);

    assert!(
        effect_resolved(&events, EffectKind::BecomePrepared),
        "reach-guard: the prepare effect must have resolved"
    );
    assert!(runner.state().stack.is_empty());
    assert!(
        !is_prepared(&runner, ineligible),
        "a creature without a prepare spell cannot become prepared"
    );
    assert!(!any_became_prepared(&events));
}

// ---------------------------------------------------------------------------
// Hexhaven Dueling Arena
// ---------------------------------------------------------------------------

/// CR 115.1c + CR 601.2c: the {2} ability may only target a creature that
/// attacked this turn. A creature that did not attack is not offered and is
/// rejected; an attacker becomes prepared.
///
/// DISCRIMINATION: reverted, no target prompt appears and the restriction is
/// lost, so neither the legal-target set nor `eligible`'s prepared state holds.
#[test]
fn hexhaven_first_ability_targets_only_attackers() {
    let LandFixture {
        mut runner,
        land,
        eligible,
        bystander,
        opponent,
        ineligible,
    } = land_fixture("Hexhaven Dueling Arena", HEXHAVEN_DUELING_ARENA, 2);
    runner
        .state_mut()
        .creatures_attacked_this_turn
        .extend([eligible, ineligible]);
    let idx = nth_prepare_ability(&runner, land, 0);
    let mut events = Vec::new();

    let legal = announce_activation(&mut runner, land, idx, &mut events);
    assert!(legal.contains(&TargetRef::Object(eligible)));
    assert!(legal.contains(&TargetRef::Object(ineligible)));
    assert!(
        !legal.contains(&TargetRef::Object(bystander)),
        "a creature that did not attack this turn is not a legal target"
    );
    assert!(!legal.contains(&TargetRef::Object(opponent)));

    assert!(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(bystander)),
            })
            .is_err(),
        "choosing a creature that did not attack must be rejected"
    );
    choose_and_resolve(&mut runner, eligible, &mut events);

    assert!(
        is_prepared(&runner, eligible),
        "the chosen attacker must become prepared"
    );
    assert!(became_prepared(&events, eligible));
    assert!(!is_prepared(&runner, bystander));
}

/// CR 722.3a: an attacker without a prepare spell is a legal target of the
/// {2} ability but does not become prepared.
#[test]
fn hexhaven_first_ability_ineligible_attacker_stays_unprepared() {
    let LandFixture {
        mut runner,
        land,
        eligible,
        ineligible,
        ..
    } = land_fixture("Hexhaven Dueling Arena", HEXHAVEN_DUELING_ARENA, 2);
    runner
        .state_mut()
        .creatures_attacked_this_turn
        .extend([eligible, ineligible]);
    let idx = nth_prepare_ability(&runner, land, 0);
    let mut events = Vec::new();

    let legal = announce_activation(&mut runner, land, idx, &mut events);
    assert!(legal.contains(&TargetRef::Object(ineligible)));
    choose_and_resolve(&mut runner, ineligible, &mut events);

    assert!(
        effect_resolved(&events, EffectKind::BecomePrepared),
        "reach-guard: the prepare effect must have resolved"
    );
    assert!(runner.state().stack.is_empty());
    assert!(!is_prepared(&runner, ineligible));
    assert!(!is_prepared(&runner, eligible));
    assert!(!any_became_prepared(&events));
}

/// The {4} ability has no attack restriction: a creature that did not attack
/// is a legal target and becomes prepared.
#[test]
fn hexhaven_second_ability_targets_any_creature() {
    let LandFixture {
        mut runner,
        land,
        eligible,
        bystander,
        ..
    } = land_fixture("Hexhaven Dueling Arena", HEXHAVEN_DUELING_ARENA, 4);
    let idx = nth_prepare_ability(&runner, land, 1);
    let mut events = Vec::new();

    let legal = announce_activation(&mut runner, land, idx, &mut events);
    assert!(legal.contains(&TargetRef::Object(bystander)));
    choose_and_resolve(&mut runner, bystander, &mut events);

    assert!(
        is_prepared(&runner, bystander),
        "the chosen creature must become prepared"
    );
    assert!(became_prepared(&events, bystander));
    assert!(!is_prepared(&runner, eligible));
}

// ---------------------------------------------------------------------------
// Biblioplex Tomekeeper
// ---------------------------------------------------------------------------

struct TomekeeperFixture {
    runner: GameRunner,
    tomekeeper: ObjectId,
    eligible: ObjectId,
    bystander: ObjectId,
    ineligible: ObjectId,
}

fn tomekeeper_fixture() -> TomekeeperFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tomekeeper = scenario
        .add_creature_to_hand_from_oracle(P0, "Biblioplex Tomekeeper", 3, 4, BIBLIOPLEX_TOMEKEEPER)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let eligible = scenario.add_creature(P0, "Eligible Scholar", 2, 2).id();
    let bystander = scenario.add_creature(P1, "Bystander Scholar", 2, 2).id();
    let ineligible = scenario.add_creature(P0, "Plain Bear", 2, 2).id();
    let mut runner = scenario.build();
    // Hostile fixture: the source itself is eligible, so a "prepares its own
    // source" fallback would be observable.
    for id in [tomekeeper, eligible, bystander] {
        give_prepare_face(&mut runner, id);
    }
    TomekeeperFixture {
        runner,
        tomekeeper,
        eligible,
        bystander,
        ineligible,
    }
}

/// Cast Tomekeeper and drive its ETB trigger: select `modes`, answer the
/// target prompt with `target`, then pass priority until the stack is empty.
/// Returns every emitted event and the legal targets of the target prompt, if
/// one appeared. Modeled on the Arbalest Engineers / Sauron drivers.
fn drive_tomekeeper_etb(
    runner: &mut GameRunner,
    tomekeeper: ObjectId,
    modes: &[usize],
    target: Option<ObjectId>,
) -> (Vec<GameEvent>, Option<Vec<TargetRef>>) {
    let card_id = runner.state().objects[&tomekeeper].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: tomekeeper,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Biblioplex Tomekeeper must succeed")
        .events;
    let mut chose_mode = false;
    let mut prompt = None;
    let mut remaining_target = target;
    for _ in 0..200 {
        let result = match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => runner
                .act(GameAction::OrderTriggers { order: vec![0] })
                .expect("order triggers"),
            WaitingFor::AbilityModeChoice { .. } => {
                chose_mode = true;
                runner
                    .act(GameAction::SelectModes {
                        indices: modes.to_vec(),
                    })
                    .expect("select modes")
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                prompt = current_target_prompt(runner);
                let target = remaining_target
                    .take()
                    .expect("a target prompt requires a declared target");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose target")
            }
            WaitingFor::Priority { .. } => {
                if chose_mode && runner.state().stack.is_empty() {
                    return (events, prompt);
                }
                runner.act(GameAction::PassPriority).expect("pass priority")
            }
            other => panic!(
                "unexpected WaitingFor while driving Tomekeeper's ETB trigger: {}",
                other.variant_name()
            ),
        };
        events.extend(result.events);
    }
    panic!("Tomekeeper's ETB trigger did not resolve within the step budget");
}

/// CR 603.3d + CR 601.2c + CR 722.3a: the prepare mode targets a creature as
/// the trigger goes on the stack; that creature becomes prepared and the
/// trigger's source does not.
///
/// DISCRIMINATION: reverted, the mode has no target slot (no prompt), and the
/// `ParentTarget` fallback prepares Tomekeeper instead of `eligible`.
#[test]
fn tomekeeper_prepare_mode_prepares_the_chosen_creature() {
    let TomekeeperFixture {
        mut runner,
        tomekeeper,
        eligible,
        bystander,
        ineligible,
    } = tomekeeper_fixture();

    let (events, prompt) = drive_tomekeeper_etb(&mut runner, tomekeeper, &[0], Some(eligible));

    let legal = prompt.expect("CR 115.1d: the prepare mode must declare a target slot");
    assert!(legal.contains(&TargetRef::Object(eligible)));
    assert!(
        is_prepared(&runner, eligible),
        "the chosen creature must become prepared"
    );
    assert!(became_prepared(&events, eligible));
    assert!(
        !is_prepared(&runner, tomekeeper),
        "the trigger source must not become prepared"
    );
    assert!(!is_prepared(&runner, bystander));
    assert!(!is_prepared(&runner, ineligible));
}

/// CR 722.3b: the unprepare mode removes the designation from the chosen
/// creature only.
///
/// DISCRIMINATION: reverted, there is no prompt and the source (not prepared)
/// is "unprepared" instead, so `eligible` stays prepared.
#[test]
fn tomekeeper_unprepare_mode_unprepares_the_chosen_creature() {
    let TomekeeperFixture {
        mut runner,
        tomekeeper,
        eligible,
        bystander,
        ..
    } = tomekeeper_fixture();
    pre_prepare(&mut runner, eligible);
    pre_prepare(&mut runner, bystander);

    let (events, prompt) = drive_tomekeeper_etb(&mut runner, tomekeeper, &[1], Some(eligible));

    let legal = prompt.expect("CR 115.1d: the unprepare mode must declare a target slot");
    assert!(legal.contains(&TargetRef::Object(eligible)));
    assert!(
        !is_prepared(&runner, eligible),
        "the chosen creature must become unprepared"
    );
    assert!(became_unprepared(&events, eligible));
    assert!(
        is_prepared(&runner, bystander),
        "an unchosen prepared creature stays prepared"
    );
    assert!(!is_prepared(&runner, tomekeeper));
}

/// CR 722.3a: in the prepare mode, a legal target with no prepare spell does
/// not become prepared. Reach-guards: the prompt offered it and the effect
/// resolved.
#[test]
fn tomekeeper_prepare_mode_ineligible_target_stays_unprepared() {
    let TomekeeperFixture {
        mut runner,
        tomekeeper,
        ineligible,
        ..
    } = tomekeeper_fixture();

    let (events, prompt) = drive_tomekeeper_etb(&mut runner, tomekeeper, &[0], Some(ineligible));

    let legal = prompt.expect("CR 115.1d: the prepare mode must declare a target slot");
    assert!(legal.contains(&TargetRef::Object(ineligible)));
    assert!(
        effect_resolved(&events, EffectKind::BecomePrepared),
        "reach-guard: the prepare effect must have resolved"
    );
    assert!(runner.state().stack.is_empty());
    assert!(!is_prepared(&runner, ineligible));
    assert!(!is_prepared(&runner, tomekeeper));
    assert!(!any_became_prepared(&events));
}

/// CR 700.2: "choose up to one" with no mode chosen changes nothing.
/// Regression guard (passes before and after the fix).
#[test]
fn tomekeeper_choosing_no_mode_changes_nothing() {
    let TomekeeperFixture {
        mut runner,
        tomekeeper,
        eligible,
        ..
    } = tomekeeper_fixture();

    let (events, prompt) = drive_tomekeeper_etb(&mut runner, tomekeeper, &[], None);

    assert!(prompt.is_none(), "no mode chosen means no target prompt");
    assert!(!is_prepared(&runner, tomekeeper));
    assert!(!is_prepared(&runner, eligible));
    assert!(!any_became_prepared(&events));
}

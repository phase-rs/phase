//! Regression for issue #5973 — Bloodthirsty Blade equipped to an opponent's creature.
//!
//! Oracle:
//!   Equipped creature gets +2/+0 and is goaded. (...)
//!   {1}: Attach this Equipment to target creature an opponent controls.
//!   Activate only as a sorcery.
//!
//! CR 701.15b: the goading player is the Equipment's controller ("you"), not the
//! equipped creature's controller. Equipping an opponent's creature must:
//!   1. pump the host (+2/+0),
//!   2. force the host to attack each combat if able,
//!   3. force the host to attack a player other than the Equipment controller
//!      if able — and still allow attacking the goader when that clause is
//!      unsatisfiable (2-player "if able").

use engine::game::combat::{
    attacker_constraints_for_active_player, creature_must_attack, get_valid_attacker_ids,
    validate_attack_declaration, AttackTarget, CombatRequirement,
};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityKind, ContinuousModification, ControllerRef, Effect, TargetFilter,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const BLOODTHIRSTY_BLADE: &str = "\
Equipped creature gets +2/+0 and is goaded. (It attacks each combat if able and attacks a player other than you if able.)\n\
{1}: Attach this Equipment to target creature an opponent controls. Activate only as a sorcery.";

fn refresh(runner: &mut engine::game::scenario::GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

fn attach(runner: &mut engine::game::scenario::GameRunner, equipment: ObjectId, host: ObjectId) {
    let state = runner.state_mut();
    state.objects.get_mut(&equipment).unwrap().attached_to = Some(host.into());
    state
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .push(equipment);
    state.layers_dirty.mark_full();
}

fn setup_two_player() -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);

    let host = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let blade = scenario
        .add_creature(P0, "Bloodthirsty Blade", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(BLOODTHIRSTY_BLADE)
        .id();

    let mut runner = scenario.build();
    attach(&mut runner, blade, host);
    refresh(&mut runner);
    runner.state_mut().active_player = P1;
    (runner, blade, host)
}

fn setup_three_player() -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::DeclareAttackers);

    let host = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let blade = scenario
        .add_creature(P0, "Bloodthirsty Blade", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(BLOODTHIRSTY_BLADE)
        .id();

    let mut runner = scenario.build();
    attach(&mut runner, blade, host);
    refresh(&mut runner);
    runner.state_mut().active_player = P1;
    (runner, blade, host)
}

#[test]
fn bloodthirsty_blade_parse_emits_single_goaded_without_continuous_graft() {
    let parsed = engine::parser::parse_oracle_text(
        BLOODTHIRSTY_BLADE,
        "Bloodthirsty Blade",
        &[],
        &["Artifact".to_string()],
        &["Equipment".to_string()],
    );
    let goaded: Vec<_> = parsed
        .statics
        .iter()
        .filter(|sd| sd.mode == StaticMode::Goaded)
        .collect();
    assert_eq!(
        goaded.len(),
        1,
        "expected exactly one Goaded static, got {:?}",
        parsed.statics
    );

    // Continuous must NOT also AddStaticMode(Goaded): that grafts a SelfRef
    // Goaded onto the host, whose source.controller is the HOST — wrongly
    // treating the opponent as a second goader (issue #5973 crash class).
    let continuous_goad_graft = parsed.statics.iter().any(|sd| {
        sd.mode == StaticMode::Continuous
            && sd.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::AddStaticMode {
                        mode: StaticMode::Goaded
                    }
                )
            })
    });
    assert!(
        !continuous_goad_graft,
        "Continuous must not also AddStaticMode(Goaded); that double-goads via host controller"
    );
}

#[test]
fn bloodthirsty_blade_activated_attach_parses() {
    let parsed = engine::parser::parse_oracle_text(
        BLOODTHIRSTY_BLADE,
        "Bloodthirsty Blade",
        &[],
        &["Artifact".to_string()],
        &["Equipment".to_string()],
    );
    let attach = parsed
        .abilities
        .iter()
        .find(|a| matches!(a.effect.as_ref(), Effect::Attach { .. }))
        .unwrap_or_else(|| {
            panic!(
                "expected Attach activated ability, got {:?}",
                parsed.abilities
            )
        });
    assert!(
        matches!(attach.kind, AbilityKind::Activated),
        "attach must be activated, got {:?}",
        attach.kind
    );
    let Effect::Attach { target, .. } = attach.effect.as_ref() else {
        unreachable!()
    };
    match target {
        TargetFilter::Typed(tf) => {
            assert_eq!(
                tf.controller,
                Some(ControllerRef::Opponent),
                "attach target must be opponent-controlled, got {tf:?}"
            );
        }
        other => panic!("expected Typed opponent-creature filter, got {other:?}"),
    }
}

#[test]
fn bloodthirsty_blade_pump_applies_to_opponent_host() {
    let (mut runner, _blade, host) = setup_two_player();
    refresh(&mut runner);

    let obj = &runner.state().objects[&host];
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(4), Some(2)),
        "Equipped creature gets +2/+0: 2/2 -> 4/2"
    );
}

#[test]
fn bloodthirsty_blade_forces_opponent_host_to_attack() {
    let (mut runner, _blade, host) = setup_two_player();
    refresh(&mut runner);

    assert!(
        creature_must_attack(runner.state(), host),
        "equipped opponent's creature must attack each combat if able"
    );

    let valid = get_valid_attacker_ids(runner.state());
    assert!(valid.contains(&host), "host must be a legal attacker");

    let constraints = attacker_constraints_for_active_player(runner.state(), &valid);
    assert!(
        matches!(
            constraints.get(&host),
            Some(CombatRequirement::MustAttack { .. })
        ),
        "display constraints must surface MustAttack for the host, got {:?}",
        constraints.get(&host)
    );
}

#[test]
fn bloodthirsty_blade_two_player_attack_into_goader_is_legal() {
    // CR 701.15b "if able": in 2-player, the only legal attack target IS the
    // goading player, so the away-from clause is unsatisfiable and attacking
    // the goader must still be legal (and required by the generic must-attack).
    let (mut runner, _blade, host) = setup_two_player();
    refresh(&mut runner);

    assert!(
        validate_attack_declaration(runner.state(), &[(host, AttackTarget::Player(P0))], &[])
            .is_ok(),
        "attacking the only available player (the goader) must be legal when \
         the away-from clause is unsatisfiable"
    );

    assert!(
        validate_attack_declaration(runner.state(), &[], &[]).is_err(),
        "omitting the goaded host must fail the CR 508.1d requirement bar"
    );
}

#[test]
fn bloodthirsty_blade_three_player_must_attack_away_from_equipment_controller() {
    // Behavioral goader attribution: P0 controls the Blade, host is P1's.
    // During P1's combat, attacking P0 (the Equipment controller) must be
    // illegal while P2 is available; attacking P2 must be legal.
    let (mut runner, _blade, host) = setup_three_player();
    refresh(&mut runner);

    let attacks_goader =
        validate_attack_declaration(runner.state(), &[(host, AttackTarget::Player(P0))], &[]);
    assert!(
        attacks_goader.is_err(),
        "CR 701.15b: must attack a player other than the Equipment controller when able; \
         attacking P0 while P2 is available must fail, got {attacks_goader:?}"
    );

    let attacks_other =
        validate_attack_declaration(runner.state(), &[(host, AttackTarget::Player(P2))], &[]);
    assert!(
        attacks_other.is_ok(),
        "attacking a non-goader (P2) must be legal, got {attacks_other:?}"
    );
}

#[test]
fn bloodthirsty_blade_activate_attach_to_opponent_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let host = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let blade = scenario
        .add_creature(P0, "Bloodthirsty Blade", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(BLOODTHIRSTY_BLADE)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(ManaUnit::new(ManaType::Colorless, blade, false, vec![]));

    let attach_idx = runner.state().objects[&blade]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::Attach { .. }))
        .expect("Bloodthirsty Blade attach activated ability");

    runner
        .activate(blade, attach_idx)
        .target_objects(&[host])
        .resolve();

    assert_eq!(
        runner.state().objects[&blade].attached_to,
        Some(host.into()),
        "Blade must attach to the opponent's creature; blade zone={:?}",
        runner.state().objects[&blade].zone
    );
    assert!(
        runner.state().objects[&host].attachments.contains(&blade),
        "host must list the Blade as an attachment"
    );
    assert_eq!(runner.state().objects[&blade].zone, Zone::Battlefield);

    // After attach, layers must apply goad without panicking.
    refresh(&mut runner);
    runner.state_mut().active_player = P1;
    assert!(
        creature_must_attack(runner.state(), host),
        "after activate-attach, host must be goaded into attacking"
    );
}

use engine::ai_support::{
    adversarial_swarm_witness, SwarmWitnessIndeterminate, SwarmWitnessResult,
};
#[cfg(feature = "test-support")]
use engine::ai_support::{adversarial_swarm_witness_with_counters, SwarmWitnessCounters};
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, DamageModification, Effect, QuantityExpr,
    ReplacementDefinition, StaticCondition, StaticDefinition, TargetFilter, TriggerDefinition,
    TypeFilter, TypedFilter, UnlessPayScaling,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;

fn attacks(ids: &[ObjectId]) -> Vec<(ObjectId, AttackTarget)> {
    ids.iter()
        .copied()
        .map(|id| (id, AttackTarget::Player(P1)))
        .collect()
}

#[cfg(feature = "test-support")]
#[test]
fn swarm_witness_fails_fast_after_multiblock_replacement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Flyer", 1, 1).flying().id();
    for _ in 0..12 {
        scenario.add_creature(P1, "Reach", 0, 2).reach();
    }
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let mut counters = SwarmWitnessCounters::default();
    assert_eq!(
        adversarial_swarm_witness_with_counters(
            runner.state(),
            P0,
            &attacks(&[attacker]),
            &mut counters
        ),
        SwarmWitnessResult::Indeterminate(SwarmWitnessIndeterminate::TriggerOrReplacement),
    );
    assert_eq!(
        counters,
        SwarmWitnessCounters {
            root_clone_applies: 1,
            raw_leaves: 4,
            legal_leaves: 4,
            candidate_clone_applies: 4,
        },
        "the unsupported double-block leaf must stop streaming before later declarations clone or apply"
    );
}

#[cfg(feature = "test-support")]
#[test]
fn swarm_witness_caps_before_enumerating_many_flying_attackers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attackers: Vec<_> = (0..4096)
        .map(|_| scenario.add_creature(P0, "Flyer", 1, 1).flying().id())
        .collect();
    scenario.add_creature(P1, "Reach", 0, 2).reach();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let mut counters = SwarmWitnessCounters::default();
    assert_eq!(
        adversarial_swarm_witness_with_counters(
            runner.state(),
            P0,
            &attacks(&attackers),
            &mut counters
        ),
        SwarmWitnessResult::Indeterminate(SwarmWitnessIndeterminate::DeclarationCap),
    );
    assert_eq!(
        counters,
        SwarmWitnessCounters {
            root_clone_applies: 1,
            ..Default::default()
        }
    );
}

#[cfg(feature = "test-support")]
#[test]
fn swarm_witness_reaches_equal_count_fliers_against_ground_blockers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attackers: Vec<_> = (0..3)
        .map(|_| scenario.add_creature(P0, "Flyer", 1, 1).flying().id())
        .collect();
    for _ in 0..3 {
        scenario.add_creature(P1, "Ground", 0, 2);
    }
    let mut runner = scenario.build();
    runner.state_mut().players[P1.0 as usize].life = 3;
    runner.advance_to_combat();
    let declared_attacks = attacks(&attackers);
    let mut counters = SwarmWitnessCounters::default();
    let result = adversarial_swarm_witness_with_counters(
        runner.state(),
        P0,
        &declared_attacks,
        &mut counters,
    );
    let SwarmWitnessResult::Certified(witness) = result else {
        panic!("equal-count flying alpha must certify: {result:?}");
    };
    assert_eq!(witness.resulting_life_loss, 3);
    assert!(witness.is_lethal);
    assert!(witness.binds_declaration(runner.state(), &declared_attacks));
    assert_eq!(
        counters,
        SwarmWitnessCounters {
            root_clone_applies: 1,
            raw_leaves: 1,
            legal_leaves: 1,
            candidate_clone_applies: 1,
        },
        "the reducer-autodeclared empty block must replay exactly one leaf without a second declaration"
    );
}

/// CR 508.1 / CR 509.1 / CR 510.1b-c: the witness measures the reducer's
/// actual life change after the defender's least-damaging legal block.
#[test]
fn swarm_witness_certifies_lethal_after_worst_legal_block() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attackers: Vec<_> = (0..3)
        .map(|_| scenario.add_creature(P0, "Bear", 3, 3).id())
        .collect();
    let blocker = scenario.add_creature(P1, "Wall", 0, 4).id();
    let mut runner = scenario.build();
    runner.state_mut().players[P1.0 as usize].life = 5;
    runner.advance_to_combat();

    let result = adversarial_swarm_witness(runner.state(), P0, &attacks(&attackers));
    let SwarmWitnessResult::Certified(witness) = result else {
        panic!("ordinary one-blocker combat must produce a witness: {result:?}");
    };
    assert_eq!(witness.defending_player, P1);
    assert_eq!(witness.defending_life_before, 5);
    assert_eq!(witness.resulting_life_loss, 6);
    assert!(witness.is_lethal);
    assert!(witness.binds_declaration(runner.state(), &attacks(&attackers)));
    assert_eq!(witness.worst_declaration.len(), 1);
    assert_eq!(witness.worst_declaration[0].0.object_id, blocker);
    assert!(
        witness.resulting_life_loss >= 5,
        "revert guard: omitting the reducer-backed defender block can overstate lethal"
    );
}

/// The best legal defense blocks one bear, leaving six damage; that is not
/// enough to defeat a player at seven life, so a caller must not promote it.
#[test]
fn swarm_witness_reports_near_miss_from_actual_life_loss() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attackers: Vec<_> = (0..3)
        .map(|_| scenario.add_creature(P0, "Bear", 3, 3).id())
        .collect();
    scenario.add_creature(P1, "Wall", 0, 4);
    let mut runner = scenario.build();
    runner.state_mut().players[P1.0 as usize].life = 7;
    runner.advance_to_combat();

    let result = adversarial_swarm_witness(runner.state(), P0, &attacks(&attackers));
    let SwarmWitnessResult::Certified(witness) = result else {
        panic!("ordinary one-blocker combat must produce a witness: {result:?}");
    };
    assert_eq!(witness.resulting_life_loss, 6);
    assert!(!witness.is_lethal);
    assert!(witness.resulting_life_loss < 7);
}

/// Exponential declaration sets are refused before the AI can make a swarm claim.
#[test]
fn swarm_witness_fails_closed_on_declaration_cap() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attackers: Vec<_> = (0..13)
        .map(|_| scenario.add_creature(P0, "Bear", 1, 1).id())
        .collect();
    for _ in 0..13 {
        scenario.add_creature(P1, "Wall", 0, 4);
    }
    let mut runner = scenario.build();
    runner.advance_to_combat();

    assert_eq!(
        adversarial_swarm_witness(runner.state(), P0, &attacks(&attackers)),
        SwarmWitnessResult::Indeterminate(SwarmWitnessIndeterminate::DeclarationCap),
        "revert guard: an unbounded blocker cartesian product must never certify lethal"
    );
}

/// Input-shape exclusions fail closed before this helper makes a combat claim.
#[test]
fn swarm_witness_fails_closed_on_topology_invalid_step_and_nonplayer_target() {
    let mut multiplayer = GameScenario::new_n_player(3, 42);
    multiplayer.at_phase(Phase::PreCombatMain);
    let mut multiplayer = multiplayer.build();
    multiplayer.advance_to_combat();
    assert_eq!(
        adversarial_swarm_witness(multiplayer.state(), P0, &[]),
        SwarmWitnessResult::Indeterminate(SwarmWitnessIndeterminate::UnsupportedTopology)
    );

    let mut precombat = GameScenario::new();
    precombat.at_phase(Phase::PreCombatMain);
    let precombat = precombat.build();
    assert_eq!(
        adversarial_swarm_witness(precombat.state(), P0, &[]),
        SwarmWitnessResult::Indeterminate(SwarmWitnessIndeterminate::InvalidAttack)
    );

    let mut empty_declaration = GameScenario::new();
    empty_declaration.at_phase(Phase::PreCombatMain);
    empty_declaration.add_creature(P0, "Bear", 3, 3);
    let mut empty_declaration = empty_declaration.build();
    empty_declaration.advance_to_combat();
    assert!(matches!(
        empty_declaration.state().waiting_for,
        engine::types::game_state::WaitingFor::DeclareAttackers { .. }
    ));
    assert_eq!(
        adversarial_swarm_witness(empty_declaration.state(), P0, &[]),
        SwarmWitnessResult::Indeterminate(SwarmWitnessIndeterminate::InvalidAttack)
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Bear", 3, 3).id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    assert_eq!(
        adversarial_swarm_witness(
            runner.state(),
            P0,
            &[(attacker, AttackTarget::Planeswalker(ObjectId(999)))]
        ),
        SwarmWitnessResult::Indeterminate(SwarmWitnessIndeterminate::NonPlayerTarget)
    );
}

/// CR 509.1b / CR 510.1c: a trample assignment or an additional-blocker
/// declaration adds a choice the bounded witness deliberately does not make.
#[test]
fn swarm_witness_fails_closed_on_damage_and_extra_block_choices() {
    let mut trample = GameScenario::new();
    trample.at_phase(Phase::PreCombatMain);
    let trampler = trample.add_creature(P0, "Trampler", 3, 3).trample().id();
    let mut trample = trample.build();
    trample.advance_to_combat();
    assert_eq!(
        adversarial_swarm_witness(trample.state(), P0, &attacks(&[trampler])),
        SwarmWitnessResult::Indeterminate(SwarmWitnessIndeterminate::DamageChoice)
    );

    let mut multiblock = GameScenario::new();
    multiblock.at_phase(Phase::PreCombatMain);
    let attacker = multiblock.add_creature(P0, "Bear", 3, 3).id();
    let blocker = multiblock.add_creature(P1, "Guard", 0, 4).id();
    let mut multiblock = multiblock.build();
    multiblock
        .state_mut()
        .objects
        .get_mut(&blocker)
        .expect("fixture blocker")
        .static_definitions
        .push(StaticDefinition::new(StaticMode::ExtraBlockers {
            count: Some(1),
        }));
    multiblock.advance_to_combat();
    assert_eq!(
        adversarial_swarm_witness(multiblock.state(), P0, &attacks(&[attacker])),
        SwarmWitnessResult::Indeterminate(SwarmWitnessIndeterminate::MultiBlockChoice)
    );
}

/// CR 508.1h / CR 603.3: a combat tax payment or an attack trigger introduces
/// a prompt/stack continuation that the bounded witness must not silently choose.
#[test]
fn swarm_witness_fails_closed_on_combat_tax_and_attack_trigger() {
    let mut tax = GameScenario::new();
    tax.at_phase(Phase::PreCombatMain);
    let attacker = tax.add_creature(P0, "Bear", 3, 3).id();
    let tax_source = tax.add_creature(P1, "Taxer", 0, 4).id();
    let mut tax = tax.build();
    tax.state_mut()
        .objects
        .get_mut(&tax_source)
        .expect("fixture tax source")
        .static_definitions
        .push(
            StaticDefinition::new(StaticMode::CantAttack)
                .affected(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: Some(ControllerRef::Opponent),
                    properties: vec![],
                }))
                .condition(StaticCondition::UnlessPay {
                    cost: ManaCost::generic(1),
                    scaling: UnlessPayScaling::Flat,
                    defended: None,
                }),
        );
    tax.advance_to_combat();
    assert_eq!(
        adversarial_swarm_witness(tax.state(), P0, &attacks(&[attacker])),
        SwarmWitnessResult::Indeterminate(SwarmWitnessIndeterminate::CostOrPrompt)
    );

    let mut trigger = GameScenario::new();
    trigger.at_phase(Phase::PreCombatMain);
    let attacker = trigger.add_creature(P0, "Trigger Bear", 3, 3).id();
    let mut trigger = trigger.build();
    trigger
        .state_mut()
        .objects
        .get_mut(&attacker)
        .expect("fixture attacker")
        .push_printed_trigger(
            TriggerDefinition::new(TriggerMode::Attacks)
                .valid_card(TargetFilter::SelfRef)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                )),
        );
    trigger.advance_to_combat();
    assert_eq!(
        adversarial_swarm_witness(trigger.state(), P0, &attacks(&[attacker])),
        SwarmWitnessResult::Indeterminate(SwarmWitnessIndeterminate::TriggerOrReplacement)
    );
}

/// CR 510.1 + CR 614.1a: combat damage is assigned after blockers, and an
/// applicable replacement effect is a real reducer boundary, so the bounded
/// witness abstains before it could measure unreplaced damage.
#[test]
fn swarm_witness_fails_closed_on_combat_damage_replacement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Amplified Bear", 3, 3).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&attacker)
        .expect("fixture attacker")
        .replacement_definitions
        .push(
            ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .damage_modification(DamageModification::Double),
        );
    runner.advance_to_combat();

    assert_eq!(
        adversarial_swarm_witness(runner.state(), P0, &attacks(&[attacker])),
        SwarmWitnessResult::Indeterminate(SwarmWitnessIndeterminate::TriggerOrReplacement),
        "a live combat-damage replacement must prevent a raw-damage swarm certificate"
    );
}

/// CR 614.1b: a replacement that applies as combat advances changes the normal
/// reducer path, so the bounded witness declines rather than certifying it.
#[test]
fn swarm_witness_fails_closed_on_replacement_applied_during_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Bear", 3, 3).id();
    let phase_skipper = scenario.add_creature(P1, "Phase Skipper", 0, 4).id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let mut skip_declare_blockers = ReplacementDefinition::new(ReplacementEvent::BeginPhase);
    skip_declare_blockers.consume_on_apply = true;
    let phase_skipper = runner
        .state_mut()
        .objects
        .get_mut(&phase_skipper)
        .expect("fixture phase skipper");
    phase_skipper
        .replacement_definitions
        .push(skip_declare_blockers.clone());
    std::sync::Arc::make_mut(&mut phase_skipper.base_replacement_definitions)
        .push(skip_declare_blockers);

    assert_eq!(
        adversarial_swarm_witness(runner.state(), P0, &attacks(&[attacker])),
        SwarmWitnessResult::Indeterminate(SwarmWitnessIndeterminate::TriggerOrReplacement),
        "a replacement applied while advancing combat must prevent a swarm certificate"
    );
}

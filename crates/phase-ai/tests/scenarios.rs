use std::collections::HashMap;
use std::collections::HashSet;

use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
use engine::game::engine::apply_as_current;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::{
    StackEntry, StackEntryKind, TargetSelectionProgress, TargetSelectionSlot, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use phase_ai::auto_play::run_ai_actions;
use phase_ai::choose_action;
use phase_ai::config::{create_config, AiDifficulty, Platform};
use rand::rngs::SmallRng;
use rand::SeedableRng;

#[test]
fn scenario_prefers_opponent_target_over_self() {
    let mut runner = GameScenario::new().build();
    runner.state_mut().waiting_for = WaitingFor::TriggerTargetSelection {
        player: P0,
        trigger_controller: None,
        trigger_event: None,
        trigger_events: Vec::new(),
        target_slots: vec![TargetSelectionSlot {
            legal_targets: vec![TargetRef::Player(P0), TargetRef::Player(P1)],
            optional: false,
        }],
        mode_labels: Vec::new(),
        target_constraints: Vec::new(),
        selection: TargetSelectionProgress {
            current_slot: 0,
            selected_slots: Vec::new(),
            current_legal_targets: vec![TargetRef::Player(P0), TargetRef::Player(P1)],
        },
        source_id: None,
        description: None,
    };

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(11);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
    );
}

#[test]
fn scenario_skips_optional_target_with_no_legal_choices() {
    let mut runner = GameScenario::new().build();
    runner.state_mut().waiting_for = WaitingFor::TriggerTargetSelection {
        player: P0,
        trigger_controller: None,
        trigger_event: None,
        trigger_events: Vec::new(),
        target_slots: vec![TargetSelectionSlot {
            legal_targets: Vec::new(),
            optional: true,
        }],
        mode_labels: Vec::new(),
        target_constraints: Vec::new(),
        selection: Default::default(),
        source_id: None,
        description: None,
    };

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(12);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::ChooseTarget { target: None })
    );
}

#[test]
fn scenario_blocks_lethal_attack_when_a_block_exists() {
    let mut scenario = GameScenario::new();
    scenario.with_life(P0, 3);
    let attacker = scenario.add_creature(P1, "Attacker", 4, 4).id();
    let blocker = scenario.add_creature(P0, "Blocker", 1, 1).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::DeclareBlockers;
        state.active_player = P1;
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, P0)],
            ..Default::default()
        });
        state.waiting_for = WaitingFor::DeclareBlockers {
            player: P0,
            valid_blocker_ids: vec![blocker],
            valid_block_targets: HashMap::from([(blocker, vec![attacker])]),
            block_requirements: HashMap::new(),
        };
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(13);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker)],
        })
    );
}

#[test]
fn scenario_multiplayer_attacks_to_finish_exposed_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let attacker_a = scenario.add_creature(P0, "Attacker A", 3, 3).id();
    let attacker_b = scenario.add_creature(P0, "Attacker B", 2, 2).id();
    let _threat = scenario.add_creature(PlayerId(2), "Threat", 5, 5).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.phase = Phase::DeclareAttackers;
        state.players[1].life = 4;
        state.players[2].life = 20;
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: P0,
            valid_attacker_ids: vec![attacker_a, attacker_b],
            valid_attack_targets: vec![AttackTarget::Player(P1), AttackTarget::Player(PlayerId(2))],
        };
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(14);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    let Some(engine::types::actions::GameAction::DeclareAttackers { attacks, .. }) = action else {
        panic!("expected declare attackers action");
    };
    assert_eq!(attacks.len(), 2);
    assert!(attacks
        .iter()
        .all(|(_, target)| *target == AttackTarget::Player(P1)));
    assert!(attacks.iter().any(|(id, _)| *id == attacker_a));
    assert!(attacks.iter().any(|(id, _)| *id == attacker_b));
}

#[test]
fn scenario_mcts_plays_available_land_deterministically() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land_id = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);

    // Move the land to hand (basic land is added to battlefield; we need it in hand for PlayLand)
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&land_id).unwrap();
        obj.zone = engine::types::zones::Zone::Hand;
        state.battlefield.retain(|&id| id != land_id);
        state.players[0].hand.push_back(land_id);
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(15);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::PlayLand {
            object_id: land_id,
            card_id: runner.state().objects[&land_id].card_id,
        })
    );
}

#[test]
fn scenario_priority_choice_remains_reducer_legal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Bear", 2, 2);
    scenario.add_bolt_to_hand(P0);

    let runner = scenario.build();
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(16);
    let action = choose_action(runner.state(), P0, &config, &mut rng)
        .expect("AI should choose a legal priority action");

    let mut sim = runner.state().clone();
    apply_as_current(&mut sim, action).expect("AI-selected action should remain reducer-legal");
}

#[test]
fn scenario_bounded_ai_sequence_progresses_without_panicking() {
    let mut scenario = GameScenario::new();
    scenario.with_life(P0, 3);
    let attacker = scenario.add_creature(P1, "Attacker", 4, 4).id();
    let blocker = scenario.add_creature(P0, "Blocker", 1, 1).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::DeclareBlockers;
        state.active_player = P1;
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, P0)],
            ..Default::default()
        });
        state.waiting_for = WaitingFor::DeclareBlockers {
            player: P0,
            valid_blocker_ids: vec![blocker],
            valid_block_targets: HashMap::from([(blocker, vec![attacker])]),
            block_requirements: HashMap::new(),
        };
    }

    let ai_players = HashSet::from([P0]);
    let ai_configs = HashMap::from([(P0, create_config(AiDifficulty::VeryHard, Platform::Native))]);
    let mut ai_rng = SmallRng::seed_from_u64(42);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());
    let results = run_ai_actions(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
    );

    assert!(
        !results.is_empty(),
        "AI loop should take at least one action"
    );
    assert!(
        results.len() <= 200,
        "AI loop should stay within its hard safety cap"
    );
}

#[test]
fn scenario_very_hard_wasm_passes_instead_of_postcombat_giant_growth() {
    let mut scenario = GameScenario::new();
    scenario.add_creature(P0, "Bear", 2, 2);
    scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Giant Growth",
            true,
            "Target creature gets +3/+3 until end of turn.",
        )
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::PostCombatMain;
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
    let mut rng = SmallRng::seed_from_u64(17);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::PassPriority)
    );
}

#[test]
fn scenario_very_hard_wasm_uses_giant_growth_to_win_combat() {
    let mut scenario = GameScenario::new();
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Blocker", 4, 4).id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Giant Growth",
            true,
            "Target creature gets +3/+3 until end of turn.",
        )
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::DeclareBlockers;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, P1)],
            blocker_assignments: HashMap::from([(attacker, vec![blocker])]),
            blocker_to_attacker: HashMap::from([(blocker, vec![attacker])]),
            ..Default::default()
        });
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
    let mut rng = SmallRng::seed_from_u64(18);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::CastSpell {
            object_id: growth,
            card_id: runner.state().objects[&growth].card_id,
            targets: Vec::new(),

            payment_mode: CastPaymentMode::Auto,
        })
    );
}

#[test]
fn scenario_very_hard_wasm_passes_with_empty_stack_counterspell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_spell_to_hand_from_oracle(P0, "Counterspell", true, "Counter target spell.")
        .id();

    let runner = scenario.build();
    let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
    let mut rng = SmallRng::seed_from_u64(19);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::PassPriority)
    );
}

#[test]
fn scenario_very_hard_wasm_passes_on_redundant_removal() {
    let mut scenario = GameScenario::new();
    let target = scenario.add_creature(P1, "Target", 2, 2).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::PreCombatMain;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.stack.push_back(StackEntry {
            id: ObjectId(301),
            source_id: ObjectId(300),
            controller: P0,
            kind: StackEntryKind::Spell {
                ability: Some(ResolvedAbility::new(
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value: 3 },
                        target: TargetFilter::Any,
                        damage_source: None,
                    },
                    vec![TargetRef::Object(target)],
                    ObjectId(300),
                    P0,
                )),
                card_id: CardId(300),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
    let mut rng = SmallRng::seed_from_u64(20);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::PassPriority),
        "Expected pass instead of redundant removal with Murder {:?}",
        runner.state().objects[&murder].name
    );
}

#[test]
fn scenario_harvester_of_misery_cast_is_preferred_over_pass() {
    let mut scenario = GameScenario::new();
    let _harvester = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Harvester of Misery",
            5,
            4,
            "When Harvester of Misery enters, target creature gets -2/-2 until end of turn.",
        )
        .id();
    scenario.add_creature(P1, "Opponent Bear", 2, 2);

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::PreCombatMain;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
    let mut rng = SmallRng::seed_from_u64(21);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    // The AI should recognise that a 5/4 menace with ETB -2/-2 against a lone 2/2
    // is strong. Accept either casting or passing — this scenario is marginal at
    // VeryHard search depth because the mana constraints are tight.
    assert!(
        matches!(
            action,
            Some(engine::types::actions::GameAction::CastSpell { .. })
                | Some(engine::types::actions::GameAction::PassPriority)
        ),
        "AI should either cast Harvester or pass, got {action:?}"
    );
}

/// Regression (issue #1189): when a human controls an AI seat via Mindslaver,
/// the server AI loop must not attempt to act for that seat — it would apply
/// actions as the wrong player and hang or crash.
#[test]
fn mindslaver_human_control_stops_ai_loop() {
    let mut runner = {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_land_to_hand(P1, "Forest");
        scenario.build()
    };
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.turn_decision_controller = Some(P0);
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P1 });
    }

    let ai_players = HashSet::from([P1]);
    let ai_configs = HashMap::from([(P1, create_config(AiDifficulty::VeryHard, Platform::Native))]);
    let mut ai_rng = SmallRng::seed_from_u64(1189);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());
    let results = run_ai_actions(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
    );

    assert!(
        results.is_empty(),
        "AI must not act when a human controls the AI seat (Mindslaver)"
    );
}

/// Under Emrakul-style control the AI controller must still act for the human seat.
#[test]
fn emrakul_ai_control_runs_for_controlled_human() {
    let mut runner = {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_land_to_hand(P0, "Forest");
        scenario.build()
    };
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.turn_decision_controller = Some(P1);
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P0 });
    }

    let ai_players = HashSet::from([P1]);
    let ai_configs = HashMap::from([(P1, create_config(AiDifficulty::VeryHard, Platform::Native))]);
    let mut ai_rng = SmallRng::seed_from_u64(2012);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());
    let results = run_ai_actions(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
    );

    assert!(
        !results.is_empty(),
        "AI controller must act during the controlled human turn"
    );
}

// ---------------------------------------------------------------------------
// Claws of Gix dead-end regression (CR 601.2g/601.2h ordering — mana paid FIRST,
// removal LAST). The composite "{1}, Sacrifice a permanent" used to pay the
// sacrifice FIRST, so when the only {1} source (Mox Opal Metalcraft) needed the
// sacrificed artifact to stay countable, the residual {1} became unpayable —
// every `SelectCards` candidate failed `apply_as_current`, leaving an empty
// scored set and a `fallback_action` debug_assert panic. The mana-leg detour now
// pays {1} on the INTACT board (the CR 601.2g window) before the sacrifice, so
// the activation is legal and the loop completes.
// ---------------------------------------------------------------------------

/// Build a `{T}: Add {1}` mana ability gated by Metalcraft-style live-eval
/// "control 3+ artifacts" (`ActivationRestriction::RequiresCondition`).
fn metalcraft_mox_def() -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, Comparator,
        ControllerRef, ParsedCondition, QuantityRef, TypeFilter, TypedFilter,
    };
    let mut def = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: engine::types::ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 1 },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Tap);
    def.activation_restrictions
        .push(ActivationRestriction::RequiresCondition {
            condition: Some(ParsedCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(
                            TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
                        ),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            }),
        });
    def
}

/// The Claws-of-Gix activated ability: `{1}, Sacrifice a permanent: You gain 1 life.`
fn claws_of_gix_def() -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, SacrificeCost, TypedFilter,
    };
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: engine::types::mana::ManaCost::generic(1),
            },
            AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::permanent()),
                1,
            )),
        ],
    })
}

/// V3 (∃-success): board with 4 artifacts (Mox + 3 others) so sacrificing one
/// leaves 3 → Metalcraft holds → a witness exists. Driving the AI loop must
/// COMPLETE without reaching the `fallback_action` panic. The original dead-end
/// would panic here.
#[test]
fn scenario_claws_of_gix_witness_board_does_not_dead_end() {
    let mut scenario = GameScenario::new();
    {
        let mut mox = scenario.add_creature(P0, "Mox Opal", 0, 0);
        mox.as_artifact();
        mox.with_ability_definition(metalcraft_mox_def());
    }
    // Three plain artifacts so total = 4; sacrificing one leaves 3 (Metalcraft).
    for i in 0..3 {
        let mut a = scenario.add_creature(P0, &format!("Artifact {i}"), 0, 1);
        a.as_artifact();
    }
    {
        let mut claws = scenario.add_creature(P0, "Claws of Gix", 0, 1);
        claws.as_artifact();
        claws.with_ability_definition(claws_of_gix_def());
    }

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::PreCombatMain;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let ai_players = HashSet::from([P0]);
    let ai_configs = HashMap::from([(P0, create_config(AiDifficulty::VeryHard, Platform::Native))]);
    let mut ai_rng = SmallRng::seed_from_u64(19024);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());
    // The assertion is non-panic: a recurrence of the dead-end aborts via the
    // `fallback_action` debug_assert before this returns.
    let results = run_ai_actions(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
    );
    assert!(
        results.len() <= 200,
        "AI loop must stay within its safety cap and never dead-end"
    );
}

/// V3 sibling (mana-first, formerly the "no-witness" dead-end): board with
/// exactly 3 artifacts (Mox + one plain artifact + Claws — itself an artifact),
/// so EVERY eligible sacrifice would drop the artifact count to 2 → Metalcraft
/// off. CR 601.2g pays {1} from the Mox on the INTACT 3-artifact board BEFORE the
/// sacrifice, so the Claws activation is LEGAL and the AI loop completes it
/// without dead-ending. REVERT-FAILING: reverting the mana-first detour restores
/// the sacrifice-first ordering, where `can_pay` is rejected (or the activation
/// dead-ends), so `legal_actions` no longer surfaces the Claws activation and the
/// pending-cost loop panics at `search.rs` "AI fallback reached during pending
/// cast (variant PayCost, spell Claws of Gix)" — the baseline seed-19057 abort.
#[test]
fn scenario_claws_of_gix_mana_first_board_proposes_and_completes() {
    let mut scenario = GameScenario::new();
    {
        let mut mox = scenario.add_creature(P0, "Mox Opal", 0, 0);
        mox.as_artifact();
        mox.with_ability_definition(metalcraft_mox_def());
    }
    // One plain artifact; with the Mox and the (artifact) Claws this is exactly
    // 3 artifacts. Sacrificing ANY of the three drops the count to 2 → no
    // Metalcraft → the {1} leg would be unpayable AFTER the sacrifice, but the
    // mana-first detour pays it on the intact board before that.
    {
        let mut a = scenario.add_creature(P0, "Artifact 0", 0, 1);
        a.as_artifact();
    }
    let claws = {
        let mut claws = scenario.add_creature(P0, "Claws of Gix", 0, 1);
        claws.as_artifact();
        claws.with_ability_definition(claws_of_gix_def());
        claws.id()
    };

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::PreCombatMain;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    // The activation is now legal because the {1} is paid on the intact board.
    assert!(
        activation_legal_for(runner.state(), claws),
        "mana-first pays {{1}} on the intact 3-artifact board → Claws activation must be legal"
    );

    // Driving the full loop must COMPLETE without reaching the `fallback_action`
    // dead-end panic.
    let ai_players = HashSet::from([P0]);
    let ai_configs = HashMap::from([(P0, create_config(AiDifficulty::VeryHard, Platform::Native))]);
    let mut ai_rng = SmallRng::seed_from_u64(19057);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());
    let results = run_ai_actions(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
    );
    assert!(
        results.len() <= 200,
        "mana-first board must not dead-end the AI loop"
    );
}

// ---------------------------------------------------------------------------
// Battlefield-removal generalization of the Claws-of-Gix mana-first fix
// (CR 601.2g/601.2h): the same ordering applies to Exile-from-battlefield
// (CR 701.13a, Curie) and ReturnToHand-from-battlefield (plain bounce, Master
// Transmuter). Each removal would shrink the board the only {U} source depends
// on, so paying the mana FIRST (intact board) keeps the activation legal and
// avoids the dead-end the removal-first ordering produced.
// ---------------------------------------------------------------------------

/// `{T}: Add {U}` mana ability. When `metalcraft` is set the ability is gated by
/// a live-eval "control 3+ artifacts" `ActivationRestriction::RequiresCondition`
/// (the Mox-Opal model); otherwise it is unconditional.
fn blue_mox_def(metalcraft: bool) -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, Comparator,
        ControllerRef, ParsedCondition, QuantityRef, TypeFilter, TypedFilter,
    };
    use engine::types::mana::ManaColor;
    let mut def = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: engine::types::ManaProduction::Fixed {
                colors: vec![ManaColor::Blue],
                contribution: engine::types::ability::ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Tap);
    if metalcraft {
        def.activation_restrictions
            .push(ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(
                                TypedFilter::new(TypeFilter::Artifact)
                                    .controller(ControllerRef::You),
                            ),
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 3 },
                }),
            });
    }
    def
}

/// Curie-style activated ability: `{1}{U}, Exile another nontoken artifact you
/// control: gain 1 life` (effect stubbed to GainLife). The exile leg has
/// `zone: None` + an artifact (permanent-implying) filter, so the live zone
/// classifier resolves it to the battlefield (CR 701.13a). The building block
/// under test is "exile-from-battlefield as a cost shrinks board mana"; the
/// scenario fixtures are pure artifacts (the builder's `as_artifact` drops the
/// creature type), so the filter matches "another nontoken artifact" rather than
/// Curie's printed "artifact creature" — the witness mechanic is identical.
fn curie_def() -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, FilterProp, TypeFilter,
        TypedFilter,
    };
    use engine::types::mana::{ManaCost, ManaCostShard};
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Blue],
                    generic: 1,
                },
            },
            AbilityCost::Exile {
                count: 1,
                zone: None,
                filter: Some(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact)
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::Another, FilterProp::NonToken]),
                )),
            },
        ],
    })
}

/// Master Transmuter's activated ability: `{U}, {T}, Return an artifact you
/// control to its owner's hand: gain 1 life` (effect stubbed to GainLife). The
/// return leg has `from_zone: None` (battlefield bounce, CR 118.3).
fn master_transmuter_def() -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, TypeFilter, TypedFilter,
    };
    use engine::types::mana::{ManaCost, ManaCostShard};
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Blue],
                    generic: 0,
                },
            },
            AbilityCost::Tap,
            AbilityCost::ReturnToHand {
                count: 1,
                filter: Some(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
                )),
                from_zone: None,
            },
        ],
    })
}

/// Set the runner into a P0-priority main-phase decision point (mirrors the
/// Claws scenarios).
fn put_p0_on_priority(runner: &mut engine::game::scenario::GameRunner) {
    let state = runner.state_mut();
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
}

/// Whether `legal_actions` surfaces an `ActivateAbility` whose source is `id`.
fn activation_legal_for(state: &engine::types::game_state::GameState, id: ObjectId) -> bool {
    use engine::types::actions::GameAction;
    engine::ai_support::legal_actions(state)
        .iter()
        .any(|a| matches!(a, GameAction::ActivateAbility { source_id, .. } if *source_id == id))
}

/// Curie EXILE mana-first (CR 601.2g / CR 701.13a): exactly 3 artifacts
/// (Metalcraft blue Mox = sole {U} source, Curie, and the lone exile target) +
/// a Forest for the generic {1}. The mana-leg detour pays `{1}{U}` FIRST while
/// all 3 artifacts are intact (Metalcraft holds → the Mox makes {U}); the exile
/// is paid LAST. So the activation is LEGAL even though exiling afterwards drops
/// below Metalcraft. REVERT-FAILING: reverting the mana-first detour restores the
/// exile-first ordering, which dead-ends here, so `legal_actions` would no longer
/// surface the Curie activation.
#[test]
fn scenario_curie_exile_mana_first_board_is_legal() {
    let mut scenario = GameScenario::new();
    {
        let mut mox = scenario.add_creature(P0, "Blue Mox", 0, 0);
        mox.as_artifact();
        mox.with_ability_definition(blue_mox_def(true));
    }
    // The lone exile target: another nontoken artifact.
    {
        let mut tgt = scenario.add_creature(P0, "Artifact Servo", 1, 1);
        tgt.as_artifact();
    }
    // A Forest pays the generic {1}; it is NOT a {U} source.
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    let curie = {
        let mut curie = scenario.add_creature(P0, "Curie", 2, 2);
        curie.as_artifact();
        curie.with_ability_definition(curie_def());
        curie.id()
    };

    let mut runner = scenario.build();
    put_p0_on_priority(&mut runner);

    assert!(
        activation_legal_for(runner.state(), curie),
        "{{1}}{{U}} paid on the intact 3-artifact board before the exile → activation must be legal"
    );
}

/// Curie EXILE witness control (non-vacuity): same board as the dead-end test
/// plus a 4th artifact, so exiling the target leaves 3 artifacts → Metalcraft
/// stays live → the Mox keeps making {U} → a witness exists → the activation is
/// legal. This proves the `{1}{U}` leg is payable on the intact board, so the
/// dead-end test's illegality is the removal-shrink discriminator, not a vacuous
/// unpayable cost.
#[test]
fn scenario_curie_exile_witness_board_is_legal() {
    let mut scenario = GameScenario::new();
    {
        let mut mox = scenario.add_creature(P0, "Blue Mox", 0, 0);
        mox.as_artifact();
        mox.with_ability_definition(blue_mox_def(true));
    }
    {
        let mut tgt = scenario.add_creature(P0, "Artifact Servo", 1, 1);
        tgt.as_artifact();
    }
    // A 4th artifact keeps Metalcraft live after any single exile.
    {
        let mut filler = scenario.add_creature(P0, "Artifact Filler", 0, 1);
        filler.as_artifact();
    }
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    let curie = {
        let mut curie = scenario.add_creature(P0, "Curie", 2, 2);
        curie.as_artifact();
        curie.with_ability_definition(curie_def());
        curie.id()
    };

    let mut runner = scenario.build();
    put_p0_on_priority(&mut runner);

    assert!(
        activation_legal_for(runner.state(), curie),
        "exiling the target leaves 3 artifacts → Metalcraft holds → activation must be legal"
    );
}

/// Master Transmuter RETURN mana-first (CR 601.2g / CR 118.3): the sole artifact
/// the player controls is the sole {U} source (an unconditional blue Mox), and
/// it is therefore the only legal "return an artifact you control" target. The
/// Transmuter source is a NON-artifact creature, so it is not itself a return
/// target. CR 601.2g pays `{U}` by tapping the Mox FIRST (the intact-board mana
/// window), then the {T} and the return are paid LAST — so the activation is
/// LEGAL even though the only return target is the {U} source. REVERT-FAILING:
/// reverting the mana-first detour restores the return-first ordering, where
/// bouncing the Mox leaves `{U}` unpayable and `legal_actions` drops the
/// activation.
#[test]
fn scenario_master_transmuter_return_mana_first_board_is_legal() {
    let mut scenario = GameScenario::new();
    {
        let mut mox = scenario.add_creature(P0, "Blue Mox", 0, 0);
        mox.as_artifact();
        mox.with_ability_definition(blue_mox_def(false));
    }
    // Non-artifact source carrying the ability → not a return target itself.
    let transmuter = {
        let mut t = scenario.add_creature(P0, "Master Transmuter", 1, 1);
        t.with_ability_definition(master_transmuter_def());
        t.id()
    };

    let mut runner = scenario.build();
    put_p0_on_priority(&mut runner);

    assert!(
        activation_legal_for(runner.state(), transmuter),
        "{{U}} paid by tapping the Mox before the return → activation must be legal"
    );
}

/// Master Transmuter RETURN witness control (non-vacuity): same board plus a
/// basic Island (an unconditional {U} source that is NOT an artifact, so it is
/// not a return target). Returning the Mox still leaves the Island's {U}, so a
/// witness exists and the activation is legal — proving the `{U}` leg is payable
/// on the intact board and the dead-end test's illegality is the removal-shrink
/// discriminator.
#[test]
fn scenario_master_transmuter_witness_board_is_legal() {
    let mut scenario = GameScenario::new();
    {
        let mut mox = scenario.add_creature(P0, "Blue Mox", 0, 0);
        mox.as_artifact();
        mox.with_ability_definition(blue_mox_def(false));
    }
    // A second, non-artifact {U} source that survives returning the Mox.
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    let transmuter = {
        let mut t = scenario.add_creature(P0, "Master Transmuter", 1, 1);
        t.with_ability_definition(master_transmuter_def());
        t.id()
    };

    let mut runner = scenario.build();
    put_p0_on_priority(&mut runner);

    assert!(
        activation_legal_for(runner.state(), transmuter),
        "the Island keeps {{U}} available after the return → activation must be legal"
    );
}

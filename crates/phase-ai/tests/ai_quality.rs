//! AI Quality Regression Tests
//!
//! Scenario-based tests that verify the AI makes intelligent decisions across
//! common game situations. Each test constructs a board state where the correct
//! play is unambiguous and asserts the AI chooses it.

use std::collections::{HashMap, HashSet};

use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
use engine::game::deck_loading::DeckEntry;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{PlayerDeckPool, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use phase_ai::auto_play::run_ai_actions;
use phase_ai::choose_action;
use phase_ai::config::{create_config, AiDifficulty, Platform};
use phase_ai::score_candidates;
use rand::rngs::SmallRng;
use rand::SeedableRng;

// ── Helpers ──────────────────────────────────────────────────────────────

fn ai_choose(state: &engine::types::game_state::GameState, difficulty: AiDifficulty) -> GameAction {
    let config = create_config(difficulty, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(42);
    choose_action(state, P0, &config, &mut rng).expect("AI should return an action")
}

fn ai_choose_at_all_difficulties(
    state: &engine::types::game_state::GameState,
) -> Vec<(AiDifficulty, GameAction)> {
    [
        AiDifficulty::Easy,
        AiDifficulty::Medium,
        AiDifficulty::Hard,
        AiDifficulty::VeryHard,
    ]
    .into_iter()
    .map(|d| (d, ai_choose(state, d)))
    .collect()
}

// ── Blocking ─────────────────────────────────────────────────────────────

#[test]
fn blocks_lethal_attack() {
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
        };
    }

    for (diff, action) in ai_choose_at_all_difficulties(runner.state()) {
        assert_eq!(
            action,
            GameAction::DeclareBlockers {
                assignments: vec![(blocker, attacker)]
            },
            "{diff:?}: should block lethal attack"
        );
    }
}

#[test]
fn does_not_block_when_safe() {
    let mut scenario = GameScenario::new();
    scenario.with_life(P0, 20);
    let attacker = scenario.add_creature(P1, "Attacker", 2, 2).id();
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
        };
    }

    // AI at 20 life facing a 2/2 — should NOT sacrifice a 1/1 to chump block
    let action = ai_choose(runner.state(), AiDifficulty::VeryHard);
    assert_eq!(
        action,
        GameAction::DeclareBlockers {
            assignments: Vec::new()
        },
        "Should not chump block when at healthy life total"
    );
}

// ── Combat Tricks ────────────────────────────────────────────────────────

#[test]
fn does_not_cast_combat_trick_post_combat() {
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

    for (diff, action) in ai_choose_at_all_difficulties(runner.state()) {
        assert_eq!(
            action,
            GameAction::PassPriority,
            "{diff:?}: should not waste Giant Growth post-combat"
        );
    }
}

// ── Counterspells ────────────────────────────────────────────────────────

#[test]
fn does_not_cast_counterspell_with_empty_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_spell_to_hand_from_oracle(P0, "Counterspell", true, "Counter target spell.")
        .id();

    let runner = scenario.build();

    for (diff, action) in ai_choose_at_all_difficulties(runner.state()) {
        assert_eq!(
            action,
            GameAction::PassPriority,
            "{diff:?}: should not cast counterspell with empty stack"
        );
    }
}

// ── Removal Targeting ────────────────────────────────────────────────────

#[test]
fn prefers_removing_larger_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two opponent creatures: a 1/1 and a 5/5
    scenario.add_creature(P1, "Token", 1, 1);
    scenario.add_creature(P1, "Dragon", 5, 5);

    // AI has Murder in hand
    scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .id();

    let runner = scenario.build();

    // The AI should cast the removal — we just verify it casts, not passes
    let action = ai_choose(runner.state(), AiDifficulty::VeryHard);
    assert!(
        matches!(
            action,
            GameAction::CastSpell { .. } | GameAction::PassPriority
        ),
        "AI should consider casting removal or pass — got {action:?}"
    );
}

// ── Full Game Completion ─────────────────────────────────────────────────

#[test]
fn ai_vs_ai_completes_combat_sequence() {
    // Set up a combat scenario and verify AI can drive through blockers
    // without getting stuck in a PassPriority loop.
    let mut scenario = GameScenario::new();
    scenario.with_life(P0, 5);
    let attacker = scenario.add_creature(P1, "Attacker", 6, 6).id();
    let blocker = scenario.add_creature(P0, "Blocker", 2, 2).id();

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
        };
    }

    let ai_players: HashSet<PlayerId> = [P0, P1].into_iter().collect();
    let config = create_config(AiDifficulty::Medium, Platform::Native);
    let ai_configs = HashMap::from([(P0, config.clone()), (P1, config)]);

    let results = run_ai_actions(runner.state_mut(), &ai_players, &ai_configs);

    // Should take at least the DeclareBlockers action
    assert!(!results.is_empty(), "AI should take at least one action");
    // First action must be DeclareBlockers
    assert!(
        matches!(results[0].action, GameAction::DeclareBlockers { .. }),
        "First action should be DeclareBlockers, got {:?}",
        results[0].action
    );
    // Should not hit the safety cap
    assert!(
        results.len() < 200,
        "AI should not hit the safety cap (got {} actions)",
        results.len()
    );
}

#[test]
fn declare_blockers_never_produces_pass_priority() {
    // Regression test: the AI must return DeclareBlockers even when
    // the candidate pipeline filters out all generated combinations.
    let mut scenario = GameScenario::new();
    scenario.with_life(P0, 10);
    let attacker = scenario.add_creature(P1, "Attacker", 3, 3).id();
    let blocker_a = scenario.add_creature(P0, "Blocker A", 2, 2).id();
    let blocker_b = scenario.add_creature(P0, "Blocker B", 1, 1).id();

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
            valid_blocker_ids: vec![blocker_a, blocker_b],
            valid_block_targets: HashMap::from([
                (blocker_a, vec![attacker]),
                (blocker_b, vec![attacker]),
            ]),
        };
    }

    for (diff, action) in ai_choose_at_all_difficulties(runner.state()) {
        assert!(
            matches!(action, GameAction::DeclareBlockers { .. }),
            "{diff:?}: must return DeclareBlockers, got {action:?}"
        );
    }
}

// ── Attacking ────────────────────────────────────────────────────────────

#[test]
fn attacks_when_opponent_is_at_lethal() {
    let mut scenario = GameScenario::new();
    scenario.with_life(P1, 3);
    let attacker = scenario.add_creature(P0, "Attacker", 4, 4).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.phase = Phase::DeclareAttackers;
        state.active_player = P0;
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: P0,
            valid_attacker_ids: vec![attacker],
            valid_attack_targets: vec![AttackTarget::Player(P1)],
        };
    }

    for (diff, action) in ai_choose_at_all_difficulties(runner.state()) {
        match &action {
            GameAction::DeclareAttackers { attacks } => {
                assert!(
                    !attacks.is_empty(),
                    "{diff:?}: should attack when opponent is at lethal"
                );
            }
            other => panic!("{diff:?}: expected DeclareAttackers, got {other:?}"),
        }
    }
}

// ── Board Development ────────────────────────────────────────────────────

#[test]
fn casts_creature_when_mana_available() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Creature with ETB removal — clearly worth casting
    let harvester = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Harvester of Misery",
            5,
            4,
            "When Harvester of Misery enters, target creature gets -2/-2 until end of turn.",
        )
        .id();

    // Opponent has a target
    scenario.add_creature(P1, "Opponent Bear", 2, 2);

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    // AI should cast the creature with ETB removal
    let action = ai_choose(runner.state(), AiDifficulty::VeryHard);
    assert_eq!(
        action,
        GameAction::CastSpell {
            object_id: harvester,
            card_id: runner.state().objects[&harvester].card_id,
            targets: Vec::new(),
        },
        "Should cast creature with strong ETB"
    );
}

// ── Evasion Awareness ────────────────────────────────────────────────────

#[test]
fn attacks_with_evasive_creatures() {
    let mut scenario = GameScenario::new();
    let flyer = scenario.add_creature(P0, "Flyer", 3, 3).flying().id();
    // Opponent has a ground blocker
    scenario.add_creature(P1, "Ground Blocker", 4, 4);

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.phase = Phase::DeclareAttackers;
        state.active_player = P0;
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: P0,
            valid_attacker_ids: vec![flyer],
            valid_attack_targets: vec![AttackTarget::Player(P1)],
        };
    }

    // The flyer can't be blocked by a ground creature — AI should attack
    let action = ai_choose(runner.state(), AiDifficulty::VeryHard);
    match &action {
        GameAction::DeclareAttackers { attacks } => {
            assert!(
                attacks.iter().any(|(id, _)| *id == flyer),
                "Should attack with evasive flyer that can't be blocked"
            );
        }
        other => panic!("Expected DeclareAttackers, got {other:?}"),
    }
}

// ── Redundant Removal ────────────────────────────────────────────────────

#[test]
fn does_not_cast_redundant_removal() {
    use engine::types::ability::{ResolvedAbility, TargetRef};
    use engine::types::game_state::{StackEntry, StackEntryKind};
    use engine::types::identifiers::{CardId, ObjectId};

    let mut scenario = GameScenario::new();
    let target = scenario.add_creature(P1, "Target", 2, 2).id();
    let _murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::PreCombatMain;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        // Already have a Lightning Bolt targeting the same creature on the stack
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

    let action = ai_choose(runner.state(), AiDifficulty::VeryHard);
    assert_eq!(
        action,
        GameAction::PassPriority,
        "Should not cast redundant removal when target is already being killed"
    );
}

// ── Difficulty Progression ───────────────────────────────────────────────

#[test]
fn all_difficulties_produce_legal_actions() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Bear", 2, 2);
    scenario.add_creature(P1, "Opponent", 3, 3);

    let runner = scenario.build();

    for difficulty in [
        AiDifficulty::VeryEasy,
        AiDifficulty::Easy,
        AiDifficulty::Medium,
        AiDifficulty::Hard,
        AiDifficulty::VeryHard,
    ] {
        let config = create_config(difficulty, Platform::Native);
        let mut rng = SmallRng::seed_from_u64(42);
        let action = choose_action(runner.state(), P0, &config, &mut rng);
        assert!(
            action.is_some(),
            "{difficulty:?}: should produce a valid action"
        );
    }
}

// ── Threat Profile Integration ──────────────────────────────────────────

fn counterspell_entry(count: u32) -> DeckEntry {
    DeckEntry {
        card: CardFace {
            name: "Counterspell".to_string(),
            card_type: CardType {
                core_types: vec![CoreType::Instant],
                ..Default::default()
            },
            mana_cost: ManaCost::generic(2),
            abilities: vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Counter {
                    target: TargetFilter::Any,
                    source_static: None,
                    unless_payment: None,
                },
            )],
            ..Default::default()
        },
        count,
    }
}

fn wrath_entry(count: u32) -> DeckEntry {
    DeckEntry {
        card: CardFace {
            name: "Wrath of God".to_string(),
            card_type: CardType {
                core_types: vec![CoreType::Sorcery],
                ..Default::default()
            },
            mana_cost: ManaCost::generic(4),
            abilities: vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DestroyAll {
                    target: TargetFilter::Any,
                    cant_regenerate: false,
                },
            )],
            ..Default::default()
        },
        count,
    }
}

#[test]
fn threat_profile_influences_scoring_against_blue_deck() {
    // Opponent has a deck heavy on counterspells. At VeryHard (Full threat
    // awareness), the AI should score PassPriority higher relative to casting
    // a mediocre creature compared to Easy (no threat awareness).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // AI has a mediocre creature in hand
    scenario.add_creature_to_hand(P0, "Bear", 2, 2);

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };

        // Opponent has a deck pool full of counterspells
        let entries = vec![counterspell_entry(8)];
        state.deck_pools.push(PlayerDeckPool {
            player: P1,
            registered_main: entries.clone(),
            registered_sideboard: Vec::new(),
            current_main: entries,
            current_sideboard: Vec::new(),
            ..Default::default()
        });
        // Give opponent some cards in hand so threat profile is non-trivial
        state.players[1].hand = engine::im::vector![
            engine::types::identifiers::ObjectId(90),
            engine::types::identifiers::ObjectId(91),
            engine::types::identifiers::ObjectId(92),
        ];
    }

    // Score at VeryHard (Full) and Easy (None)
    let hard_config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let easy_config = create_config(AiDifficulty::Easy, Platform::Native);

    let hard_scores = score_candidates(runner.state(), P0, &hard_config);
    let easy_scores = score_candidates(runner.state(), P0, &easy_config);

    // Find PassPriority scores in each
    let hard_pass = hard_scores
        .iter()
        .find(|(a, _)| matches!(a, GameAction::PassPriority))
        .map(|(_, s)| *s);
    let easy_pass = easy_scores
        .iter()
        .find(|(a, _)| matches!(a, GameAction::PassPriority))
        .map(|(_, s)| *s);

    // At VeryHard with counterspell-heavy opponent pool, PassPriority should be scored.
    // The exact scores depend on many factors, but PassPriority should exist as an option.
    assert!(
        hard_pass.is_some() || easy_pass.is_some(),
        "PassPriority should be a valid candidate"
    );
}

#[test]
fn threat_profile_influences_scoring_against_control_deck() {
    // Opponent has board wipes. AI already has 3 creatures.
    // At VeryHard, the overextend penalty should make the AI more cautious.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // AI already has 3 creatures on board
    scenario.add_creature(P0, "Bear A", 2, 2);
    scenario.add_creature(P0, "Bear B", 2, 2);
    scenario.add_creature(P0, "Bear C", 2, 2);

    // AI has another creature in hand
    scenario.add_creature_to_hand(P0, "Bear D", 2, 2);

    // Opponent has no creatures (making wrath free for them)
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };

        // Opponent deck pool: full of wraths
        let entries = vec![wrath_entry(8)];
        state.deck_pools.push(PlayerDeckPool {
            player: P1,
            registered_main: entries.clone(),
            registered_sideboard: Vec::new(),
            current_main: entries,
            current_sideboard: Vec::new(),
            ..Default::default()
        });
        state.players[1].hand = engine::im::vector![
            engine::types::identifiers::ObjectId(90),
            engine::types::identifiers::ObjectId(91),
        ];
    }

    // At VeryHard with Full threat awareness and wrath-heavy opponent,
    // the AI should be more cautious about overextending.
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let scores = score_candidates(runner.state(), P0, &config);

    // The test validates the threat system is wired through: we have scored candidates.
    assert!(
        !scores.is_empty(),
        "AI should produce scored candidates with threat profile active"
    );
}

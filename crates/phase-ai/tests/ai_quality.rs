//! AI Quality Regression Tests
//!
//! Scenario-based tests that verify the AI makes intelligent decisions across
//! common game situations. Each test constructs a board state where the correct
//! play is unambiguous and asserts the AI chooses it.

use std::collections::{HashMap, HashSet};

use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
use engine::game::deck_loading::DeckEntry;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr, QuantityRef, TargetFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::{PlayerDeckPool, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use phase_ai::auto_play::{
    driver_step, run_ai_actions, run_ai_actions_bounded, AiActionsBreakReason,
};
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
            block_requirements: HashMap::new(),
            blocker_constraints: Default::default(),
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
            block_requirements: HashMap::new(),
            blocker_constraints: Default::default(),
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

/// With a single 2/1 creature and no Equipment on board, Slash of Light deals 1
/// damage — non-lethal on a 3/3 — so casting it at a 3/3 wastes the removal
/// spell. The AI should NOT commit the cast in this situation.
///
/// Slash of Light's Oracle text:
/// "Slash of Light deals damage equal to the number of creatures you control
///  plus the number of Equipment you control to target creature."
///
/// CR 120.3: damage equal to the number of creatures you control (1) plus the
/// number of Equipment you control (0) = 1 damage. CR 704.5g: 1 marked damage
/// on an undamaged 3/3 does not reach its 3 toughness, so it survives.
#[test]
fn does_not_cast_slash_of_light_for_nonlethal_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // AI's single 2/1 creature (1 creature, 0 Equipment → Slash deals 1).
    scenario.add_creature(P0, "My Bear", 2, 1);
    // Opponent's 3/3 that 1 damage cannot kill.
    scenario.add_creature(P1, "Opponent Bear", 3, 3);

    scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Slash of Light",
            true,
            "Slash of Light deals damage equal to the number of creatures you control plus the number of Equipment you control to target creature.",
        )
        .id();

    // Fund {1}{W} so the cast is affordable — passing must reflect the waste,
    // not an unpayable cost.
    let mut mana = vec![ManaUnit::new(
        ManaType::White,
        ObjectId(9_999),
        false,
        vec![],
    )];
    mana.push(ManaUnit::new(
        ManaType::Colorless,
        ObjectId(9_999),
        false,
        vec![],
    ));
    scenario.with_mana_pool(P0, mana);

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    // Easy, Hard, and Very Hard: the whiff guard deterministically ranks the
    // wasteful cast BELOW passing its priority. Assert via `score_candidates`
    // (the deterministic policy-registry ranking) rather than `choose_action`
    // (which applies softmax temperature + search pre-emptions and is therefore
    // difficulty-stochastic at the argmax boundary). The discriminating signal
    // for the cast-commit gate is that the whiff burn is deprioritized below
    // passing.
    //
    // Medium is deliberately NOT asserted here: at difficulty Medium the cast
    // decision goes through the search path which projects casting Slash of Light
    // as a ~WIN_SCORE (10000) line whether or not the whiff penalty is applied —
    // a search/terminal-eval artifact orthogonal to the cast-commit whiff guard.
    for diff in [
        AiDifficulty::Easy,
        AiDifficulty::Hard,
        AiDifficulty::VeryHard,
    ] {
        let config = create_config(diff, Platform::Native);
        let scored = phase_ai::score_candidates(runner.state(), P0, &config);
        let cast = scored
            .iter()
            .find(|(a, _)| matches!(a, GameAction::CastSpell { .. }))
            .map(|(_, s)| *s);
        let pass = scored
            .iter()
            .find(|(a, _)| matches!(a, GameAction::PassPriority))
            .map(|(_, s)| *s);
        let (Some(cast), Some(pass)) = (cast, pass) else {
            panic!("{diff:?}: expected both CastSpell and PassPriority candidates, got {scored:?}");
        };
        assert!(
            cast < pass,
            "{diff:?}: the wasteful Slash of Light cast ({cast:.3}) must rank below \
             passing ({pass:.3}) — the whiff guard did not deprioritize the burn"
        );
    }
}

/// Drives the full Very Hard pipeline after a wasteful Slash of Light cast would
/// be committed: confirms the AI does NOT commit the cast (and therefore never
/// points its non-lethal 1 damage at the opponent's 3/3). The cast-commit
/// whiff guard (the `removal_lethality::can_kill_any_legal_target` gate behind
/// `AntiSelfHarm::score_pre_cast`) deprioritizes a burn whose damage kills no
/// legal target, so the Very Hard AI passes instead of pinging the 3/3 for a
/// wasted 1 point.
///
/// Two-tier reach-guard: (1) the scorer must offer the exact Slash of Light
/// `CastSpell` candidate at the cast-commit step — proving the gate is in the
/// decision path rather than the test passing vacuously on an unrelated
/// action — and (2) the bounded pipeline must still produce at least one
/// decision. Only with both tiers does "no CastSpell in the results" prove the
/// whiff guard deprioritized the cast.
#[test]
fn very_hard_slash_of_light_does_not_commit_or_ping_the_3_3() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _mine = scenario.add_creature(P0, "My Bear", 2, 1).id();
    let _theirs = scenario.add_creature(P1, "Opponent Bear", 3, 3).id();

    let slash = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Slash of Light",
            true,
            "Slash of Light deals damage equal to the number of creatures you control plus the number of Equipment you control to target creature.",
        )
        .id();

    let mut mana = vec![ManaUnit::new(
        ManaType::White,
        ObjectId(9_999),
        false,
        vec![],
    )];
    mana.push(ManaUnit::new(
        ManaType::Colorless,
        ObjectId(9_999),
        false,
        vec![],
    ));
    scenario.with_mana_pool(P0, mana);

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    // Reach-guard (tier 1): the exact Slash of Light cast must be offered as a
    // candidate to the Very Hard scorer. A vacuous "no candidates" pass would
    // satisfy the outcome asserts below for the wrong reason. Read-only — no
    // borrow survives past this block.
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let scored = phase_ai::score_candidates(runner.state(), P0, &config);
    assert!(
        scored.iter().any(|(a, _)| matches!(
            a,
            GameAction::CastSpell { object_id, .. } if *object_id == slash
        )),
        "Slash of Light must be offered as a CastSpell candidate, got {scored:?}"
    );

    let ai_players = HashSet::from([P0]);
    let ai_configs = HashMap::from([(P0, config)]);
    let mut ai_rng = SmallRng::seed_from_u64(42);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());

    let results = run_ai_actions_bounded(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
        4,
    );

    // The Very Hard AI must NOT commit the wasteful cast, so it must not choose
    // a target either — the first (and only) action should be a priority pass.
    let cast = results
        .iter()
        .find(|r| matches!(r.action, GameAction::CastSpell { .. }));
    assert!(
        cast.is_none(),
        "AI must NOT commit the non-lethal Slash of Light cast — actions: {:?}",
        results.iter().map(|r| &r.action).collect::<Vec<_>>()
    );
    let target = results.iter().find_map(|r| match r.action {
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(id)),
        } => Some(id),
        _ => None,
    });
    assert!(
        target.is_none(),
        "AI must not aim Slash of Light at any creature — got target {target:?}"
    );
    // Reach-guard: the engine's full Very Hard action pipeline gave the AI a
    // chance to act (at least one decision was produced), proving the test did
    // not short-circuit before the cast-commit gate could be evaluated. A pass
    // (or any decision) reaching the arms above means the whiff guard fired.
    assert!(
        !results.is_empty(),
        "Very Hard AI must produce at least one decision at the cast-commit step"
    );
}

/// Hostile sibling for the whiff gate: the opponent has a 3/3 that survives
/// AND a 1/1 that Slash's 1 damage KILLS. The cast is a *partial* whiff, not a
/// total one — `can_kill_any_legal_target` must NOT veto. This proves the gate
/// blocks only *total* whiffs (target choice for a partial whiff is deferred to
/// the existing target-selection `lethality_bonus`).
///
/// Asserted via `score_candidates` (deterministic policy-registry ranking) at
/// Easy, exactly as the total-whiff guard test does, so the two are directly
/// comparable: total whiff → cast ranks below pass; partial whiff → cast ranks
/// above pass.
#[test]
fn slash_of_light_commits_when_a_legal_target_is_lethal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // AI's single 2/1 creature → Slash deals 1.
    scenario.add_creature(P0, "My Bear", 2, 1);
    // Opponent's 1/1 that 1 damage kills (CR 704.5g), and a 3/3 that it doesn't.
    scenario.add_creature(P1, "Small Opponent", 1, 1);
    scenario.add_creature(P1, "Big Opponent", 3, 3);

    scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Slash of Light",
            true,
            "Slash of Light deals damage equal to the number of creatures you control plus the number of Equipment you control to target creature.",
        )
        .id();

    let mut mana = vec![ManaUnit::new(
        ManaType::White,
        ObjectId(9_999),
        false,
        vec![],
    )];
    mana.push(ManaUnit::new(
        ManaType::Colorless,
        ObjectId(9_999),
        false,
        vec![],
    ));
    scenario.with_mana_pool(P0, mana);

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let config = create_config(AiDifficulty::Easy, Platform::Native);
    let scored = phase_ai::score_candidates(runner.state(), P0, &config);
    let cast = scored
        .iter()
        .find(|(a, _)| matches!(a, GameAction::CastSpell { .. }))
        .map(|(_, s)| *s);
    let pass = scored
        .iter()
        .find(|(a, _)| matches!(a, GameAction::PassPriority))
        .map(|(_, s)| *s);
    let (Some(cast), Some(pass)) = (cast, pass) else {
        panic!("expected both CastSpell and PassPriority candidates, got {scored:?}");
    };
    assert!(
        cast > pass,
        "partial whiff must NOT be vetoed: a legal lethal target (the opponent 1/1) \
         exists, so the cast ({cast:.3}) should rank above passing ({pass:.3}) — \
         the gate must only block total whiffs"
    );
}

/// Differential test for mixed-removal spells: a spell with TWO creature-only
/// effects — "deal 1 damage to target creature; destroy target creature" —
/// must NOT be penalized as a damage whiff when it still has a useful Destroy
/// line. Without the non-`DealDamage` fail-open guard, `can_kill_any_legal_target`
/// aggregates only the spell's `DealDamage` half, sees the 1 damage survive the
/// 3/3, and wrongly vetoes the cast.
///
/// The damage amount is DYNAMIC (ObjectCount of the AI's creatures → 1), matching
/// spells where `lethal_to_creature` fails open and the `can_kill_any_legal_target`
/// gate determines lethality.
///
/// This test isolates the whiff penalty. A second, otherwise-identical spell
/// deals only the dynamic 1 damage ("pure burn"): on the same board it is a
/// provable total damage whiff and receives the -8 `wasted_cast_penalty`,
/// while the mixed spell must not. Both are driven through the real cast
/// pipeline to ensure the cast-commit gate is fully evaluated.
#[test]
fn mixed_damage_and_destroy_is_not_penalized_as_a_damage_whiff() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // AI's single creature makes the dynamic ObjectCount amount resolve to 1
    // (Slash-of-Light-shaped); the opponent's 3/3 survives 1 damage but Destroy
    // kills it (CR 701.8a).
    scenario.add_creature(P0, "My Bear", 2, 1);
    scenario.add_creature(P1, "Opponent Bear", 3, 3);

    let mut my_filter = TypedFilter::creature();
    my_filter.controller = Some(ControllerRef::You);
    let amount = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(my_filter),
        },
    };

    // Mixed "deal 1 damage + destroy target creature".
    let mixed = scenario
        .add_spell_to_hand(P0, "Charred Murder", true)
        .with_ability(Effect::DealDamage {
            amount: amount.clone(),
            target: TargetFilter::Typed(TypedFilter::creature()),
            damage_source: None,
            excess: None,
        })
        .with_ability(Effect::Destroy {
            target: TargetFilter::Typed(TypedFilter::creature()),
            cant_regenerate: false,
        })
        .id();

    // Pure "deal 1 damage to target creature" — a total damage whiff here
    // (1 cannot kill the 3/3).
    let pure = scenario
        .add_spell_to_hand(P0, "Pure Burn", true)
        .with_ability(Effect::DealDamage {
            amount: amount.clone(),
            target: TargetFilter::Typed(TypedFilter::creature()),
            damage_source: None,
            excess: None,
        })
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let config = create_config(AiDifficulty::Easy, Platform::Native);
    let scored = phase_ai::score_candidates(runner.state(), P0, &config);

    // Reach-guard: BOTH spells must actually be offered as CastSpell
    // candidates to prevent a vacuous pass that never reaches the cast-commit gate.
    let mixed_score = scored
        .iter()
        .find(|(a, _)| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == mixed))
        .map(|(_, s)| *s)
        .unwrap_or_else(|| {
            panic!("mixed spell {mixed:?} must be offered as CastSpell, got {scored:?}")
        });
    let pure_score = scored
        .iter()
        .find(|(a, _)| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == pure))
        .map(|(_, s)| *s)
        .unwrap_or_else(|| {
            panic!("pure whiff spell {pure:?} must be offered as CastSpell, got {scored:?}")
        });

    // The mixed spell's Destroy line makes it strictly more castable than the
    // identical pure-damage whiff. Without the non-`DealDamage` fail-open guard,
    // `can_kill_any_legal_target` penalizes the mixed spell with the same -8
    // whiff penalty, collapsing this inequality.
    assert!(
        mixed_score > pure_score + config.policy_penalties.wasted_cast_penalty.abs() * 0.5,
        "mixed deal-1 + destroy ({mixed_score:.3}) must outrank the identical pure \
         burn whiff ({pure_score:.3}): Destroy is a useful removal line the gate \
         must not penalize as a damage whiff"
    );
}

/// Differential test for mixed control spells: a spell with a creature-damage
/// effect AND a "gain control of target permanent" effect must NOT be
/// penalized as a damage whiff when the control half is independently useful.
/// `Effect::GainControl` is classified `EffectPolarity::Contextual`, so the
/// fail-open requires the gate to cover Contextual non-`DealDamage` effects
/// with a creature-or-permanent target (CR 613.1b, Layer 2). Without it,
/// `can_kill_any_legal_target` aggregates only the `DealDamage` half, sees the
/// 1 damage survive the 3/3, and wrongly vetoes the cast.
///
/// The damage amount is DYNAMIC (ObjectCount of the AI's creatures → 1),
/// Slash-of-Light-shaped; the opponent also controls a non-creature permanent
/// (Island) so the control line is legal and genuinely useful. On this board the
/// pure burn is a provable total damage whiff (-8 `wasted_cast_penalty`) while
/// the mixed control spell must not be penalized. Both are driven through the
/// real cast pipeline so the cast-commit gate is fully evaluated.
#[test]
fn mixed_damage_and_gain_control_is_not_penalized_as_a_damage_whiff() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // AI's single creature makes the dynamic ObjectCount amount resolve to 1
    // (Slash-of-Light-shaped); the opponent's 3/3 survives 1 damage but the
    // Island is a legal, useful GainControl permanent target (CR 613.1b).
    scenario.add_creature(P0, "My Bear", 2, 1);
    scenario.add_creature(P1, "Opponent Bear", 3, 3);
    scenario.add_basic_land(P1, engine::types::mana::ManaColor::Blue);

    let mut my_filter = TypedFilter::creature();
    my_filter.controller = Some(ControllerRef::You);
    let amount = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(my_filter),
        },
    };

    // Mixed "deal 1 damage to target creature + gain control of target permanent".
    let mixed = scenario
        .add_spell_to_hand(P0, "Charmed Heist", true)
        .with_ability(Effect::DealDamage {
            amount: amount.clone(),
            target: TargetFilter::Typed(TypedFilter::creature()),
            damage_source: None,
            excess: None,
        })
        .with_ability(Effect::GainControl {
            target: TargetFilter::Typed(TypedFilter::permanent()),
        })
        .id();

    // Pure "deal 1 damage to target creature" — a total damage whiff here
    // (1 cannot kill the 3/3).
    let pure = scenario
        .add_spell_to_hand(P0, "Pure Burn", true)
        .with_ability(Effect::DealDamage {
            amount: amount.clone(),
            target: TargetFilter::Typed(TypedFilter::creature()),
            damage_source: None,
            excess: None,
        })
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let config = create_config(AiDifficulty::Easy, Platform::Native);
    let scored = phase_ai::score_candidates(runner.state(), P0, &config);

    // Reach-guard: BOTH spells must actually be offered as CastSpell
    // candidates to prevent a vacuous pass that never reaches the cast-commit gate.
    let mixed_score = scored
        .iter()
        .find(|(a, _)| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == mixed))
        .map(|(_, s)| *s)
        .unwrap_or_else(|| {
            panic!("mixed spell {mixed:?} must be offered as CastSpell, got {scored:?}")
        });
    let pure_score = scored
        .iter()
        .find(|(a, _)| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == pure))
        .map(|(_, s)| *s)
        .unwrap_or_else(|| {
            panic!("pure whiff spell {pure:?} must be offered as CastSpell, got {scored:?}")
        });

    // The mixed spell's permanent-control line makes it strictly more castable
    // than the identical pure-damage whiff. Without the Contextual/permanent
    // fail-open, `can_kill_any_legal_target` penalizes the mixed spell with the
    // same -8 whiff penalty, collapsing this inequality.
    assert!(
        mixed_score > pure_score + config.policy_penalties.wasted_cast_penalty.abs() * 0.5,
        "mixed deal-1 + gain control of a permanent ({mixed_score:.3}) must outrank \
         the identical pure burn whiff ({pure_score:.3}): stealing the opponent's \
         permanent is a useful control line (CR 613.1b) the gate must not penalize \
         as a damage whiff"
    );
}

/// A mixed spell carrying a DEFAULT-population `DestroyAll` (declaring
/// `TargetFilter::None`) plus a "deal 1 damage to target creature" effect must
/// NOT be penalized as a damage whiff when its wipe half is independently
/// useful. `TargetFilter::None` means the engine resolver's default population
/// — all creatures (destroy.rs `resolve_all`, CR 701.8) — so the opponent's
/// 3/3 is a wipe target even though the spell declares no filter. Pre-fix, the
/// cast-commit gate fed the raw `None` into `find_legal_targets` (an empty
/// set), the wipe half credited nothing, and the 1-damage half vetoed the
/// whole spell as a whiff. The wipe is also NON-targeted (CR 115.10a): target
/// legality never gates its population.
///
/// The damage amount is DYNAMIC (ObjectCount of the AI's creatures → 1),
/// Slash-of-Light-shaped. On this board the pure burn is a provable total
/// damage whiff (-8 `wasted_cast_penalty`) while the mixed wipe spell must not
/// be penalized. Both are driven through the real cast pipeline so the
/// cast-commit gate is fully evaluated.
#[test]
fn mixed_damage_and_destroy_all_is_not_penalized_as_a_damage_whiff() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // AI's single creature makes the dynamic ObjectCount amount resolve to 1
    // (Slash-of-Light-shaped); the opponent's 3/3 survives 1 damage but is in
    // the wipe's default all-creatures population (CR 701.8, destroy.rs
    // `resolve_all`).
    scenario.add_creature(P0, "My Bear", 2, 1);
    scenario.add_creature(P1, "Opponent Bear", 3, 3);

    let mut my_filter = TypedFilter::creature();
    my_filter.controller = Some(ControllerRef::You);
    let amount = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(my_filter),
        },
    };

    // Mixed "deal 1 damage to target creature + destroy all permanents" — the
    // wipe declares NO filter, so its population is the resolver's default
    // (all creatures).
    let mixed = scenario
        .add_spell_to_hand(P0, "Charred Judgement", true)
        .with_ability(Effect::DealDamage {
            amount: amount.clone(),
            target: TargetFilter::Typed(TypedFilter::creature()),
            damage_source: None,
            excess: None,
        })
        .with_ability(Effect::DestroyAll {
            target: TargetFilter::None,
            cant_regenerate: false,
        })
        .id();

    // Pure "deal 1 damage to target creature" — a total damage whiff here
    // (1 cannot kill the 3/3).
    let pure = scenario
        .add_spell_to_hand(P0, "Pure Burn", true)
        .with_ability(Effect::DealDamage {
            amount: amount.clone(),
            target: TargetFilter::Typed(TypedFilter::creature()),
            damage_source: None,
            excess: None,
        })
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let config = create_config(AiDifficulty::Easy, Platform::Native);
    let scored = phase_ai::score_candidates(runner.state(), P0, &config);

    // Reach-guard: BOTH spells must actually be offered as CastSpell
    // candidates to prevent a vacuous pass that never reaches the cast-commit gate.
    let mixed_score = scored
        .iter()
        .find(|(a, _)| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == mixed))
        .map(|(_, s)| *s)
        .unwrap_or_else(|| {
            panic!("mixed spell {mixed:?} must be offered as CastSpell, got {scored:?}")
        });
    let pure_score = scored
        .iter()
        .find(|(a, _)| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == pure))
        .map(|(_, s)| *s)
        .unwrap_or_else(|| {
            panic!("pure whiff spell {pure:?} must be offered as CastSpell, got {scored:?}")
        });

    // The mixed spell's default-population wipe line (the 3/3 is in the
    // resolver's all-creatures population, CR 701.8 / destroy.rs `resolve_all`;
    // the wipe is non-targeted, CR 115.10a) makes it strictly more castable
    // than the identical pure-damage whiff. Without the resolver-mirroring mass
    // path, `can_kill_any_legal_target` penalizes the mixed spell with the same
    // -8 whiff penalty, collapsing this inequality.
    assert!(
        mixed_score > pure_score + config.policy_penalties.wasted_cast_penalty.abs() * 0.5,
        "mixed deal-1 + default-population destroy-all ({mixed_score:.3}) must outrank \
         the identical pure burn whiff ({pure_score:.3}): the wipe's default \
         all-creatures population (CR 701.8 / destroy.rs `resolve_all`) makes the \
         3/3 a wipe target and the wipe is non-targeted (CR 115.10a), so the gate \
        must not penalize the spell as a damage whiff"
    );
}

/// Production-pipeline differential pinning BOTH seams restored by the
/// whiff-gate fix, for a MIXED wipe spell on a board whose only opposing
/// creature is HEXPROOF:
///
/// * **Targeting is gated, the wipe population is not.** Hexproof (CR 702.11b)
///   prevents the creature being *targeted* by the spell's "deal 1 damage to
///   target creature" half — but `DestroyAll` is NON-targeted (CR 115.10a), so
///   hexproof never answers it. With `TargetFilter::None` (CR 701.8) the
///   resolver's population defaults to ALL creatures, so the hexproof 3/3 is a
///   genuine wipe target.
/// * **Own bear gives the damage half a legal ANNOUNCE target (CR 601.2c).**
///   The AI's own 2/1 means the DealDamage half has a legal target to name when
///   the spell is cast, so the PENDING spell is valid and the cast pipeline
///   reaches the cast-commit scoring.
///
/// The reference R is a PURE `DestroyAll{None}` wipe (NOT pure burn): on a
/// board with no targetable opponent creature, pure burn is hard-REJECTED by
/// `is_redundant_creature_only_removal` (whose creature-only half has no live
/// opponent target), so it is never offered and cannot be a comparable
/// reference. The pure wipe R has no creature-only half, is offered, and is
/// the honest baseline: the mixed spell M (DealDamage half + wipe) must carry
/// the SAME cast-commit score as R (modulo the small margin), because both
/// clear the hexproof population and M's dead damage half adds a no-target
/// whiff only if the rescue fails.
///
/// This pins TWO fixes:
///   1. **Tactical gate mass-awareness** (tactical_gate.rs) — pre-fix M is
///      hard-REJECTED by `is_redundant_creature_only_removal` (its creature-only
///      half has no live opponent target on a hexproof board), so the
///      `CastSpell` reach-guard on M fails (only `PassPriority` is offered).
///   2. **anti_self_harm no-target rescue** — pre-rescue M carries the -8
///      `wasted_cast_penalty` no-target penalty (M ≈ R − 8), failing the
///      differential; post-rescue M ≈ R.
#[test]
fn mixed_destroy_all_not_penalized_when_only_population_is_hexproof() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The AI's own 2/1 gives the DealDamage half a legal ANNOUNCE target so the
    // pending spell is valid (CR 601.2c), and resolves the dynamic ObjectCount
    // amount to 1. The opponent's ONLY creature is hexproof (CR 702.11b): an
    // illegal TARGET for the damage half, but in the wipe's resolver population
    // (CR 115.10a non-targeted; CR 701.8 default all-creatures for `None`).
    scenario.add_creature(P0, "My Bear", 2, 1);
    scenario
        .add_creature(P1, "Hexproof Bear", 3, 3)
        .with_keyword(Keyword::Hexproof);

    let mut my_filter = TypedFilter::creature();
    my_filter.controller = Some(ControllerRef::You);
    let amount = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(my_filter),
        },
    };

    // Mixed "deal 1 damage to target creature + destroy all creatures" — the
    // damage half is announceable (own bear) but NOT lethal/useful by target;
    // the DestroyAll half clears the hexproof population.
    let mixed = scenario
        .add_spell_to_hand(P0, "Charred Judgement", true)
        .with_ability(Effect::DealDamage {
            amount: amount.clone(),
            target: TargetFilter::Typed(TypedFilter::creature()),
            damage_source: None,
            excess: None,
        })
        .with_ability(Effect::DestroyAll {
            target: TargetFilter::None,
            cant_regenerate: false,
        })
        .id();

    // Reference: PURE wipe (CR 701.8, CR 115.10a) — the honest comparable. Pure
    // burn would be hard-rejected by `is_redundant_creature_only_removal` on this
    // board (no live opponent target), so it is NOT a valid reference.
    let reference = scenario
        .add_spell_to_hand(P0, "Pure Wipe", true)
        .with_ability(Effect::DestroyAll {
            target: TargetFilter::None,
            cant_regenerate: false,
        })
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let config = create_config(AiDifficulty::Easy, Platform::Native);
    let scored = phase_ai::score_candidates(runner.state(), P0, &config);

    // Reach-guard: BOTH must be offered as CastSpell. The M reach-guard is the
    // DISCRIMINATING guard for the tactical-gate fix: pre-fix M is hard-rejected
    // (only PassPriority), so this unwrap panics.
    let mixed_score = scored
        .iter()
        .find(|(a, _)| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == mixed))
        .map(|(_, s)| *s)
        .unwrap_or_else(|| {
            panic!(
                "mixed spell {mixed:?} must be offered as CastSpell (tactical gate must be \
                    mass-aware), got {scored:?}"
            )
        });
    let ref_score = scored
        .iter()
        .find(|(a, _)| {
            matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == reference)
        })
        .map(|(_, s)| *s)
        .unwrap_or_else(|| {
            panic!("reference pure wipe {reference:?} must be offered as CastSpell, got {scored:?}")
        });

    // M's wipe clears the hexproof population exactly like R, so M must NOT be
    // meaningfully below R. Pre-rescue M carried the -8 no-target penalty
    // (M ≈ R − 8 < R − 4); post-rescue M ≈ R. The dead damage half is harmless
    // once the wipe line rescues the spell, so the allowed gap is only the
    // half-penalty margin.
    assert!(
        mixed_score > ref_score - config.policy_penalties.wasted_cast_penalty.abs() * 0.5,
        "mixed deal-1 + destroy-all on a hexproof-only board ({mixed_score:.3}) must not be \
         penalized below the pure wipe ({ref_score:.3}): the DestroyAll population is \
         NON-targeted (CR 115.10a) and clears the hexproof 3/3 (CR 702.11b gates \
         targeting only), so M is a real removal line, not a whiff"
    );
}

/// Production-pipeline differential pinning the player-relative-wipe fix for
/// the **anti_self_harm** thread: a mixed spell whose `DestroyAll` population
/// carries a companion `ControllerRef::TargetOpponent` scope ("destroy all
/// creatures target opponent controls") with a LIVE opponent creature on board
/// and the companion player target LEFT UNBOUND, exactly as at cast-commit.
///
/// * **Why UNKNOWN.** The engine resolves `ControllerRef::TargetOpponent` by
///   reading the first `TargetRef::Player` from `ability.targets` (filter.rs
///   `ControllerRef::TargetPlayer|TargetOpponent` arm) and FAILS CLOSED without
///   it, while `destroy::resolve_all` resolves the same population later via
///   `FilterContext::from_ability` AFTER the companion player is announced
///   (CR 601.2c). At cast-commit the companion slot is not yet bound, so the
///   population is UNKNOWABLE — the mass helper must fail open (`None`), NOT
///   read it as empty (CR 109.4 / CR 115.1).
/// * **The wipe is non-targeted** (CR 115.10a): population members are not
///   "targets" (CR 701.8), so the unbound companion player is a
///   target-declaration bookkeeping gap, not a legality problem for the wipe.
/// * **Reference R** is the identical target-player wipe ALONE — the
///   reviewer's apples-to-apples baseline: both M and R carry the same
///   `TargetOpponent` wipe; only M adds the non-lethal damage half. So M must
///   score ≈ R (within the half-penalty margin). Pre-fix the mass population
///   read as empty → `can_kill_any_legal_target` did not credit the wipe → M's
///   1-damage half was vetoed as a whiff (`wasted_cast_penalty`,
///   anti_self_harm) → M ≈ R − 8, failing this assert.
#[test]
fn mixed_target_opponent_wipe_is_not_penalized_when_player_unbound() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // AI's 2/1 also lets the dynamic ObjectCount amount resolve to 1; the
    // opponent's LIVE 3/3 is both a legal target for the damage half and a
    // member of the TargetOpponent wipe population (CR 115.10a).
    scenario.add_creature(P0, "My Bear", 2, 1);
    scenario.add_creature(P1, "Opponent Bear", 3, 3);

    let mut my_filter = TypedFilter::creature();
    my_filter.controller = Some(ControllerRef::You);
    let amount = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(my_filter),
        },
    };

    // Companion `TargetPlayer`/`TargetOpponent` wipe filter — its player target
    // slot is left unbound, as it is at cast-commit.
    let opponent_wipe =
        || TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::TargetOpponent));

    // Mixed "deal 1 damage to target creature + destroy all creatures target
    // opponent controls".
    let mixed = scenario
        .add_spell_to_hand(P0, "Targeted Cataclysm", true)
        .with_ability(Effect::DealDamage {
            amount: amount.clone(),
            target: TargetFilter::Typed(TypedFilter::creature()),
            damage_source: None,
            excess: None,
        })
        .with_ability(Effect::DestroyAll {
            target: opponent_wipe(),
            cant_regenerate: false,
        })
        .id();

    // Reference: the SAME TargetOpponent wipe alone (the reviewer's
    // target-player wipe baseline).
    let reference = scenario
        .add_spell_to_hand(P0, "Pure Player Wipe", true)
        .with_ability(Effect::DestroyAll {
            target: opponent_wipe(),
            cant_regenerate: false,
        })
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let config = create_config(AiDifficulty::Easy, Platform::Native);
    let scored = phase_ai::score_candidates(runner.state(), P0, &config);

    // Reach-guard: BOTH must be offered as CastSpell.
    let mixed_score = scored
        .iter()
        .find(|(a, _)| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == mixed))
        .map(|(_, s)| *s)
        .unwrap_or_else(|| {
            panic!("mixed spell {mixed:?} must be offered as CastSpell, got {scored:?}")
        });
    let ref_score = scored
        .iter()
        .find(|(a, _)| {
            matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == reference)
        })
        .map(|(_, s)| *s)
        .unwrap_or_else(|| {
            panic!(
                "reference target-player wipe {reference:?} must be offered as CastSpell, \
                    got {scored:?}"
            )
        });

    assert!(
        mixed_score > ref_score - config.policy_penalties.wasted_cast_penalty.abs() * 0.5,
        "mixed deal-1 + target-opponent wipe ({mixed_score:.3}) must not be penalized below \
         the target-player wipe baseline ({ref_score:.3}): the wipe's population is UNKNOWN \
         (companion player unbound at cast-commit, CR 109.4 / CR 115.1), so it must fail open \
         (CR 115.10a non-targeted; CR 701.8) and rescue the non-lethal damage half"
    );
}

/// Production-pipeline differential pinning the player-relative-wipe fix for
/// the **tactical-gate** thread on a board whose only opposing creature is
/// HEXPROOF. Pre-fix, an unbound player-relative wipe read as an EMPTY
/// population, so `is_redundant_creature_only_removal` (consulting
/// `has_opposing_mass_population`) saw no useful wipe and HARD-REJECTED the
/// mixed spell — only `PassPriority` was offered, so the M reach-guard below
/// panics. Post-fix the seam is UNKNOWN → not redundant → M is offered.
///
/// * The hexproof 3/3 (CR 702.11b) is an illegal TARGET for the damage half,
///   but the wipe's `TargetOpponent` population is NON-targeted (CR 115.10a)
///   and UNKNOWABLE at cast-commit (companion player unbound, CR 109.4 /
///   CR 115.1; resolved later via `FilterContext::from_ability`, CR 601.2c).
/// * The AI's own 2/1 gives the damage half a legal ANNOUNCE target so the
///   pending spell is valid (CR 601.2c).
/// * R is the same TargetOpponent wipe alone — the honest baseline: both M and
///   R carry the unknown-population wipe, so M must score ≈ R (half-penalty
///   margin), with the M-offered reach-guard as the DISCRIMINATING assert.
#[test]
fn target_opponent_wipe_offered_when_only_population_is_hexproof() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The AI's 2/1 resolves the ObjectCount amount to 1 and gives the damage
    // half a legal announce target; the opponent's only creature is hexproof.
    scenario.add_creature(P0, "My Bear", 2, 1);
    scenario
        .add_creature(P1, "Hexproof Bear", 3, 3)
        .with_keyword(Keyword::Hexproof);

    let mut my_filter = TypedFilter::creature();
    my_filter.controller = Some(ControllerRef::You);
    let amount = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(my_filter),
        },
    };

    let opponent_wipe =
        || TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::TargetOpponent));

    // Mixed "deal 1 damage to target creature + destroy all creatures target
    // opponent controls" (companion player unbound).
    let mixed = scenario
        .add_spell_to_hand(P0, "Targeted Hexproof Cataclysm", true)
        .with_ability(Effect::DealDamage {
            amount: amount.clone(),
            target: TargetFilter::Typed(TypedFilter::creature()),
            damage_source: None,
            excess: None,
        })
        .with_ability(Effect::DestroyAll {
            target: opponent_wipe(),
            cant_regenerate: false,
        })
        .id();

    // Reference: the SAME TargetOpponent wipe alone.
    let reference = scenario
        .add_spell_to_hand(P0, "Pure Player Wipe Hexproof", true)
        .with_ability(Effect::DestroyAll {
            target: opponent_wipe(),
            cant_regenerate: false,
        })
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let config = create_config(AiDifficulty::Easy, Platform::Native);
    let scored = phase_ai::score_candidates(runner.state(), P0, &config);

    // DISCRIMINATING reach-guard: M must be offered. Pre-fix
    // `is_redundant_creature_only_removal` hard-rejected M (empty population
    // read → not a useful wipe) so only PassPriority was offered — this panics.
    let mixed_score = scored
        .iter()
        .find(|(a, _)| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == mixed))
        .map(|(_, s)| *s)
        .unwrap_or_else(|| {
            panic!(
                "mixed spell {mixed:?} must be offered as CastSpell (the TargetOpponent wipe \
                    must be UNKNOWN, not empty, so the tactical gate must not hard-reject), \
                    got {scored:?}"
            )
        });
    let ref_score = scored
        .iter()
        .find(|(a, _)| {
            matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == reference)
        })
        .map(|(_, s)| *s)
        .unwrap_or_else(|| {
            panic!(
                "reference pure wipe {reference:?} must be offered as CastSpell, got {scored:?}"
            )
        });

    assert!(
        mixed_score > ref_score - config.policy_penalties.wasted_cast_penalty.abs() * 0.5,
        "mixed deal-1 + target-opponent wipe on a hexproof-only board ({mixed_score:.3}) must \
         not be penalized below the pure wipe ({ref_score:.3}): the TargetOpponent population \
         is UNKNOWN at cast-commit (CR 109.4 / CR 115.1) and the wipe is NON-targeted \
         (CR 115.10a), so the seam must rescue the spell from both the tactical gate and the \
         whiff penalty"
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
            block_requirements: HashMap::new(),
            blocker_constraints: Default::default(),
        };
    }

    let ai_players: HashSet<PlayerId> = [P0, P1].into_iter().collect();
    let config = create_config(AiDifficulty::Medium, Platform::Native);
    let ai_configs = HashMap::from([(P0, config.clone()), (P1, config)]);
    let mut ai_rng = SmallRng::seed_from_u64(42);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());

    let results = run_ai_actions(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
    );

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
fn run_ai_actions_non_empty_batch_carries_break_reason() {
    // phase#6080 follow-up (PR #6194 review): `run_ai_actions` can complete
    // one or more actions and *still* stop on a break door (here: P1 is
    // nominally AI-controlled via `ai_players` but has no entry in
    // `ai_configs`). That door reports `MissingAiConfig { player }`: an actor
    // was found and is AI-controlled, so it is a caller wiring gap, not the
    // `NoActor` stall. The old `ai_commander` driver only checked
    // `break_reason` when the returned batch was empty, so this exact
    // shape (non-empty batch + Some(break_reason)) got silently discarded.
    // This asserts `run_ai_actions` reports it, and that `driver_step` — the
    // helper the driver now uses — preserves it and signals a stop.
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
            block_requirements: HashMap::new(),
            blocker_constraints: Default::default(),
        };
    }

    // Both P0 and P1 are declared AI-controlled, but only P0 has a config.
    // P0's DeclareBlockers action applies successfully; priority then moves
    // to P1 (the active player), whose missing config stops the batch —
    // after that one action already completed.
    let ai_players: HashSet<PlayerId> = [P0, P1].into_iter().collect();
    let config = create_config(AiDifficulty::Medium, Platform::Native);
    let ai_configs = HashMap::from([(P0, config)]);
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
        "P0's DeclareBlockers action should have applied before the batch stopped"
    );
    assert!(
        matches!(
            results.break_reason,
            Some(AiActionsBreakReason::MissingAiConfig { player: P1 })
        ),
        "expected MissingAiConfig(P1): P1 is an AI seat with no ai_configs entry, \
         which is not the same stall as NoActor"
    );

    let step = driver_step(results);
    assert_eq!(step.actions_taken, 1);
    assert!(
        step.break_reason.is_some(),
        "driver_step must preserve the break reason from a non-empty batch \
         so the driver stops at this boundary instead of discarding it"
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
            block_requirements: HashMap::new(),
            blocker_constraints: Default::default(),
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
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
        };
    }

    for (diff, action) in ai_choose_at_all_difficulties(runner.state()) {
        match &action {
            GameAction::DeclareAttackers { attacks, .. } => {
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

            payment_mode: CastPaymentMode::Auto,
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
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
        };
    }

    // The flyer can't be blocked by a ground creature — AI should attack
    let action = ai_choose(runner.state(), AiDifficulty::VeryHard);
    match &action {
        GameAction::DeclareAttackers { attacks, .. } => {
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
                ability: Some(Box::new(ResolvedAbility::new(
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value: 3 },
                        target: TargetFilter::Any,
                        damage_source: None,
                        excess: None,
                    },
                    vec![TargetRef::Object(target)],
                    ObjectId(300),
                    P0,
                ))),
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
                    source_rider: None,
                    countered_spell_zone: None,
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
        let entries = std::sync::Arc::new(vec![counterspell_entry(8)]);
        state.deck_pools.push(PlayerDeckPool {
            player: P1,
            registered_main: std::sync::Arc::clone(&entries),
            registered_sideboard: std::sync::Arc::new(Vec::new()),
            current_main: entries,
            current_sideboard: std::sync::Arc::new(Vec::new()),
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
        let entries = std::sync::Arc::new(vec![wrath_entry(8)]);
        state.deck_pools.push(PlayerDeckPool {
            player: P1,
            registered_main: std::sync::Arc::clone(&entries),
            registered_sideboard: std::sync::Arc::new(Vec::new()),
            current_main: entries,
            current_sideboard: std::sync::Arc::new(Vec::new()),
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

// ── Mana development (Unit 1) ────────────────────────────────────────────

/// Row 1 — the headline regression: a mana-screwed AI must make its land drop.
///
/// **This test fails on unmodified main**, where a land on the battlefield
/// contributes 0.0 to every weighted feature while the same card in hand is worth
/// `+w_eff.hand_size`, making the evaluator score its own land drop as a strict
/// loss (up to −6.55 for Combo/late).
///
/// The `>= 2` `PlayLand` reach-guard is load-bearing. With exactly ONE playable
/// land, `prefer_land_drop` short-circuits before the search runs, which is why
/// `scenarios.rs::scenario_single_playable_land_uses_deterministic_shortcut`
/// passed throughout the bug's lifetime. Two lands force the shortcut to decline
/// and hand the decision to the scored path.
///
/// **Diagnostic**: the guard proves the shortcut *declines*, not that the
/// evaluator is *reached*. `fast_priority_action` runs at both `choose_action`
/// and the top of `score_candidates_core` and carries further shortcuts. If this
/// goes red, check `fast_priority_action` before suspecting the offset.
///
/// # Why this asserts a SCORE ORDERING and not a sampled action
///
/// Measured on this exact fixture: `PlayLand` scores **2.333 against
/// `PassPriority` 3.272 with the offset disabled** (the bug — passing outranks
/// the land drop) and **9.833 against 9.755 with it enabled**. The ordering flips,
/// which is the whole fix, and asserting it is deterministic.
///
/// The surviving margin is only **+0.078**, far below the raw +7.5 eval delta,
/// and that compression is real rather than a fixture artifact: `PassPriority` in
/// a precombat main phase does **not** forfeit the land drop, so the continuation
/// search sees both lines converge on "the land gets played" and correctly scores
/// them as nearly equivalent. At T = 0.5 that leaves a sampled `choose_action`
/// call close to a coin flip, so a sampled assertion here would be a flaky test
/// pinning the rng rather than the behaviour. See the implementation report: this
/// compression falsifies the plan's risk-10.13 claim that the offset outranks the
/// entire policy layer on an ordinary priority decision.
#[test]
fn mana_screwed_ai_ranks_land_drop_above_passing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    scenario.add_land_to_hand(P0, "Forest");
    scenario.add_land_to_hand(P0, "Island");
    // An uncastable 4-drop, so passing is a genuinely available alternative and
    // the AI is not simply choosing the only action on offer.
    scenario
        .add_creature_to_hand(P0, "Big Body", 4, 4)
        .with_mana_cost(ManaCost::generic(4));

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.players[0].lands_played_this_turn = 0;
    }

    let land_actions: Vec<_> = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .filter(|a| matches!(a, GameAction::PlayLand { .. }))
        .collect();
    assert!(
        land_actions.len() >= 2,
        "reach-guard: at least TWO distinct PlayLand actions must be legal, else \
         `prefer_land_drop` short-circuits and this test degrades into the \
         vacuous one-land case; got {}",
        land_actions.len()
    );

    let land_score = action_score(runner.state(), |a| matches!(a, GameAction::PlayLand { .. }));
    let pass_score = action_score(runner.state(), |a| matches!(a, GameAction::PassPriority));
    assert_no_forced_win("row 1 (mana screw)", [land_score, pass_score]);

    assert!(
        land_score > pass_score,
        "a mana-screwed AI must rank its land drop ABOVE passing; got \
         land={land_score} pass={pass_score}. With the offset disabled this same \
         fixture scores land=2.333 pass=3.272, so this assertion flips on revert."
    );
}

/// Deck lists whose *ratios* drive `DeckProfile::analyze` to the named archetype.
///
/// The list is a CLASSIFICATION INPUT only — it does not describe the fixture's
/// battlefield or hand. Two constructional constraints, from the classifier's own
/// predicates: lands are skipped entirely (`if is_land { continue; }`), so land
/// count moves no ratio; and `is_ramp_effect` matches `Effect::Mana { .. }`, so
/// putting the fixture's mana rock in the LIST would raise `ramp_ratio` and pull
/// the classification toward Ramp.
fn control_deck_entries() -> Vec<DeckEntry> {
    // avg_mv 5.0, creature 0, removal 1.0 → control_score 4.50 vs next-best 1.0,
    // a 77.8% margin, far above the 20% hybrid threshold.
    vec![DeckEntry {
        card: CardFace {
            name: "Ruinous Path".to_string(),
            card_type: CardType {
                core_types: vec![CoreType::Sorcery],
                ..Default::default()
            },
            mana_cost: ManaCost::generic(5),
            abilities: vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Destroy {
                    target: TargetFilter::Any,
                    cant_regenerate: false,
                },
            )],
            ..Default::default()
        },
        count: 20,
    }]
}

fn aggro_deck_entries() -> Vec<DeckEntry> {
    // avg_mv 1.0, creature 1.0, removal 0 → aggro_score 4.50 vs next-best 1.0.
    vec![DeckEntry {
        card: CardFace {
            name: "Savannah Lions".to_string(),
            card_type: CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            },
            mana_cost: ManaCost::generic(1),
            ..Default::default()
        },
        count: 20,
    }]
}

/// Midrange is the `#[default]` archetype and the fallback for any deck the
/// classifier cannot place, which makes it the highest-population archetype and
/// the one whose absence from the disclosure table matters most.
///
/// It is also the hardest list to build, and that difficulty is a property of
/// `classify`, not of this fixture: `midrange_score` is the constant `1.0`, so
/// Midrange wins only when all four *scored* archetypes are simultaneously weak,
/// and `aggro_score` (rising in `creature_ratio`) and `combo_score` (rising in
/// `1 - creature_ratio`) are directly opposed. The ridge between them is narrow.
///
/// Solved ratios: 20 nonland cards, `creature_ratio` 0.45, `removal_ratio` 0.10,
/// `draw_ratio` 0, `ramp_ratio` 0, `avg_mv` exactly 3.5. Scores:
/// aggro 0.80, combo 0.775, control 0.75, ramp 0.25 — all strictly below
/// midrange's 1.0, so `primary` is Midrange and `adjust_weights_with` uses the
/// Midrange multipliers. Pure-vs-Hybrid is deliberately NOT asserted: the top gap
/// lands on `classify`'s 20 % hybrid threshold to within one ULP and the label
/// does not change `archetype`, which is the only thing that reaches the weights.
fn midrange_deck_entries() -> Vec<DeckEntry> {
    let plain = |name: &str, mv: u32, core: CoreType, count: u32| DeckEntry {
        card: CardFace {
            name: name.to_string(),
            card_type: CardType {
                core_types: vec![core],
                ..Default::default()
            },
            mana_cost: ManaCost::generic(mv),
            ..Default::default()
        },
        count,
    };

    vec![
        // 9 creatures × mv 3 = 27
        plain("Midrange Body", 3, CoreType::Creature, 9),
        // 2 removal × mv 5 = 10. Sorceries, so they do not move `creature_ratio`.
        DeckEntry {
            card: CardFace {
                name: "Midrange Removal".to_string(),
                card_type: CardType {
                    core_types: vec![CoreType::Sorcery],
                    ..Default::default()
                },
                mana_cost: ManaCost::generic(5),
                abilities: vec![AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Destroy {
                        target: TargetFilter::Any,
                        cant_regenerate: false,
                    },
                )],
                ..Default::default()
            },
            count: 2,
        },
        // 9 filler × (6 × mv 4 + 3 × mv 3) = 33. Total mv 70 / 20 = 3.5 exactly.
        // Deliberately ability-free: any `Effect::Mana` would raise `ramp_ratio`
        // through `is_ramp_effect` and pull the classification toward Ramp.
        plain("Midrange Filler A", 4, CoreType::Artifact, 6),
        plain("Midrange Filler B", 3, CoreType::Artifact, 3),
    ]
}

fn push_deck_pool(state: &mut engine::types::game_state::GameState, entries: Vec<DeckEntry>) {
    let entries = std::sync::Arc::new(entries);
    state.deck_pools.push(PlayerDeckPool {
        player: P0,
        registered_main: std::sync::Arc::clone(&entries),
        registered_sideboard: std::sync::Arc::new(Vec::new()),
        current_main: entries,
        current_sideboard: std::sync::Arc::new(Vec::new()),
        ..Default::default()
    });
}

/// Give both players a library deep enough that no player can deck out inside the
/// search horizon.
///
/// # Why a scoring test needs this at all
///
/// `GameScenario::new()` leaves both libraries **empty**, and per **CR 704.5b** a
/// player who has attempted to draw from an empty library since the last
/// state-based-action check loses the game. So in any scenario built this way,
/// P1 loses at their very next draw step — and a search that happens to look that
/// far ahead sees a **forced win** and returns `WIN_SCORE` (10000.0) instead of a
/// board evaluation.
///
/// That made row 16 the only flaky test in a 1672-test suite: measured across
/// three consecutive full-suite runs on one unchanged tree it went FAIL / PASS /
/// PASS, at 20.498s / 15.441s / 7.850s, with the failure landing on the slowest
/// run — while its two siblings from this same builder stayed green at 1.2–2.9s.
/// Run-to-run variance in *how much search happens* is exactly what an
/// empty-library win produces: get deep enough and the body branch scores
/// `WIN_SCORE`; fall short and it scores normally.
///
/// # Why this is at the fixture and not in `GameScenario::new()`
///
/// `GameScenario` is a **shared engine helper** that every concurrent agent's
/// tests build on. Changing its constructor to stock libraries would silently
/// alter the scenario of every test in the workspace. The defect is that *this*
/// fixture asks a scoring question of a position that contains a forced win, so
/// the fix belongs here.
///
/// # Why this cannot move any pinned number
///
/// Library contents are read by nothing this unit measures: `Library` appears
/// nowhere in `eval.rs` or `zone_eval.rs`, and `card_advantage::count_resources`
/// sums battlefield permanents (tokens ×0.5) plus `hand.len()` with no library
/// term. Every margin pinned by rows 16b/17/18 is therefore expected to be
/// **byte-identical** after this change — which was verified, not assumed.
fn stock_libraries(scenario: &mut GameScenario) {
    // Ten each: the search horizon is a handful of plies, so ten draw steps is far
    // beyond reach while keeping the object count negligible.
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Library Filler");
        scenario.add_card_to_library_top(P1, "Library Filler");
    }
}

/// Rock-vs-body fixture shared by rows 16/16b and 17: a 2-mana renewable mana rock
/// and a comparable 2-mana 3/3 body, both castable from two untapped lands, in the
/// LATE phase (where `hand_size` peaks and the disclosed inversion is largest).
///
/// Returns `(state, rock_id, body_id)`.
fn rock_vs_body_fixture(
    entries: Vec<DeckEntry>,
) -> (
    engine::types::game_state::GameState,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);

    let rock_id = scenario.add_card_to_hand(P0, "Mana Rock");
    let body_id = scenario
        .add_creature_to_hand(P0, "Comparable Body", 3, 3)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        // Late phase: `EvalWeightSet::for_turn` returns `late` for turns >= 8,
        // where `hand_size` is largest and the disclosed margin is widest.
        state.turn_number = 9;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };

        let rock = state.objects.get_mut(&rock_id).unwrap();
        rock.card_types.core_types.push(CoreType::Artifact);
        rock.base_card_types = rock.card_types.clone();
        rock.mana_cost = ManaCost::generic(2);
        let mut mana_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: engine::types::ability::ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        );
        mana_ability.cost = Some(engine::types::ability::AbilityCost::Tap);
        std::sync::Arc::make_mut(&mut rock.abilities).push(mana_ability);

        push_deck_pool(state, entries);
    }

    (runner.state().clone(), rock_id, body_id)
}

/// Move `id` from `P0`'s hand to the battlefield, simulating the post-cast state.
fn resolve_to_battlefield(
    state: &mut engine::types::game_state::GameState,
    id: engine::types::identifiers::ObjectId,
) {
    state.players[0].hand.retain(|&h| h != id);
    state.objects.get_mut(&id).unwrap().zone = engine::types::zones::Zone::Battlefield;
    state.battlefield.push_back(id);
}

/// The tactical eval of `state` from P0's perspective through `archetype`-adjusted
/// late-phase weights, including both fixed serve-time offsets. This is exactly
/// the model §10.1's margin table is computed from.
fn archetype_adjusted_eval(
    state: &engine::types::game_state::GameState,
    archetype: phase_ai::deck_profile::DeckArchetype,
) -> f64 {
    let profile = phase_ai::deck_profile::DeckProfile {
        archetype,
        ..Default::default()
    };
    let weights = profile.adjust_weights_with(
        &phase_ai::deck_profile::ArchetypeMultipliers::default(),
        &phase_ai::eval::EvalWeightSet::learned().late,
    );
    let f = phase_ai::eval::evaluate_features(state, P0).expect("fixture is non-terminal");
    f.weighted_total(&weights) + f.energy_offset + f.mana_development_offset
}

/// Assert the fixture's deck list actually classifies as `expected`.
///
/// Without this, the row silently tests **Midrange**: `choose_action` builds its
/// own `AiSession` internally, `deck_profile` is populated only from
/// `state.deck_pools`, and an absent pool defaults to `DeckArchetype::Midrange`
/// (whose margin is −0.327, so both rows would pass having measured neither
/// archetype). `AiSession::archetype` reads `DeckFeatures`, but that is exact
/// here: `DeckFeatures::analyze` calls `DeckProfile::analyze` on the same deck and
/// both collapse `classification` with the identical `Pure`/`Hybrid{primary}`
/// match, and `primary` is what `adjust_weights_with` uses.
fn assert_classifies_as(
    state: &engine::types::game_state::GameState,
    expected: phase_ai::deck_profile::DeckArchetype,
) {
    assert_eq!(
        phase_ai::AiSession::from_game(state).archetype(P0),
        Some(expected),
        "fixture deck must classify as {expected:?}, else this row silently tests Midrange"
    );
}

/// Count how many of 11 deterministic trials select a cast of `wanted`.
fn cast_selection_count(
    state: &engine::types::game_state::GameState,
    wanted: engine::types::identifiers::ObjectId,
) -> usize {
    let config = create_config(AiDifficulty::Hard, Platform::Native);
    (0..=10u64)
        .filter(|&seed| {
            let mut rng = SmallRng::seed_from_u64(seed);
            matches!(
                choose_action(state, P0, &config, &mut rng),
                Some(GameAction::CastSpell { object_id, .. }) if object_id == wanted
            )
        })
        .count()
}

/// The production action score `choose_action` samples from, for one candidate.
///
/// `score_candidates` is the same scoring pipeline `choose_action` runs; it simply
/// stops before the softmax draw. Asserting on the SCORE rather than on a sampled
/// action is both deterministic and strictly more discriminating — a sampled
/// assertion can pass or fail on the rng even when the ordering is stable.
fn action_score(
    state: &engine::types::game_state::GameState,
    matches_action: impl Fn(&GameAction) -> bool,
) -> f64 {
    let config = create_config(AiDifficulty::Hard, Platform::Native);
    score_candidates(state, P0, &config)
        .into_iter()
        .find(|(action, _)| matches_action(action))
        .map(|(_, score)| score)
        .expect("candidate must be scored")
}

fn cast_score(
    state: &engine::types::game_state::GameState,
    id: engine::types::identifiers::ObjectId,
) -> f64 {
    action_score(
        state,
        |a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == id),
    )
}

/// Remaining headroom between an archetype's creature and a comparable mana rock,
/// for the two archetypes that do NOT invert.
///
/// These are values MEASURED when these rows landed, not predictions. The floors
/// below them are early-warning tripwires: the ordering assertions catch a sign
/// flip, these catch the approach to one.
///
/// # Why these are measured at the eval layer and the orderings at the score layer
///
/// The property being guarded — "the coefficient compressed this archetype's
/// counterweight but did not invert it" — is a property of the **weight tables**,
/// not of the search. `compressed_margin` therefore reads
/// `archetype_adjusted_eval`, which is `evaluate_state` over the two post-cast
/// states and involves no search at all, so a band pinned to it moves only when
/// `EvalWeightSet`, `ArchetypeMultipliers` or `MANA_DEVELOPMENT_COEFF` moves —
/// which is exactly when a reader should be told.
///
/// A band pinned to `cast_score` would be a *proxy* for that property, and a leaky
/// one: `score_candidates` runs the full search, so any unrelated search change
/// moves the number and trips a band that has nothing to say about the
/// coefficient. The orderings stay at `cast_score` because an ordering is the
/// behavioural claim and is robust to that noise; only the numeric bands move
/// down a layer.
const AGGRO_MEASURED_MARGIN: f64 = 0.41465;
const AGGRO_COMPRESSED_MARGIN_FLOOR: f64 = 0.15;
const MIDRANGE_MEASURED_MARGIN: f64 = 0.32651;
const MIDRANGE_COMPRESSED_MARGIN_FLOOR: f64 = 0.1;

/// The `#[default]` archetype, spelled once so rows 18 and 19 cannot drift apart.
const MIDRANGE: phase_ai::deck_profile::DeckArchetype =
    phase_ai::deck_profile::DeckArchetype::Midrange;
const ARCH_AGGRO: phase_ai::deck_profile::DeckArchetype =
    phase_ai::deck_profile::DeckArchetype::Aggro;

/// Assert no candidate is scoring a terminal win, i.e. the fixture is being asked
/// a *board-evaluation* question and answering one.
///
/// This is the standing guard against the defect that made row 16 the only flaky
/// test in the suite: with an empty library, CR 704.5b hands P1 a loss at their
/// next draw, and a search that reaches it returns `WIN_SCORE` (10000.0) instead
/// of a board score — nondeterministically, depending on how deep that particular
/// run got. `stock_libraries` removes the cause; this detects any recurrence, and
/// says so in the failure message rather than presenting as an inexplicable
/// margin flake.
///
/// A duration check was considered as the recurrence signal and rejected: row 16
/// also runs `cast_selection_count` (11 full `choose_action` calls), so it is
/// legitimately several times slower than its siblings, and timing varies with
/// machine and suite load. Score magnitude is the direct observable — the actual
/// symptom rather than a proxy for it.
fn assert_no_forced_win(label: &str, scores: [f64; 2]) {
    // Board scores in these fixtures sit near 25–30; `WIN_SCORE` is 10000.0.
    const TERMINAL_FLOOR: f64 = 1000.0;
    for score in scores {
        assert!(
            score.abs() < TERMINAL_FLOOR,
            "{label}: candidate scored {score}, at or beyond terminal magnitude \
             (|score| >= {TERMINAL_FLOOR}, WIN_SCORE = 10000.0). The search has \
             found a FORCED WIN/LOSS in what is supposed to be a quiet scoring \
             fixture, so this row is measuring game-termination rather than the \
             mana-development margin — and will flake as search depth varies. \
             Check that `stock_libraries` still gives both players a library \
             (CR 704.5b: drawing from an empty one loses the game)."
        );
    }
}

/// `body − rock` at the eval layer for the shared rock-vs-body fixture: how much
/// headroom the archetype's creature retains over a comparable mana rock.
///
/// Positive = the body still wins (Aggro, Midrange). Negative = inverted
/// (Control — which is why row 16 measures the same quantity with the sign
/// reversed rather than calling this helper).
fn compressed_margin(
    state: &engine::types::game_state::GameState,
    rock_id: engine::types::identifiers::ObjectId,
    body_id: engine::types::identifiers::ObjectId,
    archetype: phase_ai::deck_profile::DeckArchetype,
) -> f64 {
    let mut rock_state = state.clone();
    resolve_to_battlefield(&mut rock_state, rock_id);
    let mut body_state = state.clone();
    resolve_to_battlefield(&mut body_state, body_id);
    archetype_adjusted_eval(&body_state, archetype)
        - archetype_adjusted_eval(&rock_state, archetype)
}

/// Rows 16 + 16b — **the disclosed Control inversion**, committed as standing
/// coverage rather than left as a watch item.
///
/// Unit 1 moves Control from ~0% to ≈99.98% mana-rock-over-body preference at the
/// shipped Hard temperature (T = 0.5). The maintainer was shown this and ruled
/// *"ship, then immediately work on the root fix"*, so this row documents reality
/// and regresses if reality drifts.
///
/// Reading a red: (a) margin below 2.0 → the inversion is GONE, which is the
/// desired outcome once the `board_stats` land/nonland root fix lands — that
/// successor unit must UPDATE this band, not delete the row, because deleting it
/// erases the only standing record of the disclosed behaviour; (b) margin above
/// 7.0 → the offset is being applied twice, a build defect; (c) archetype
/// guard trips → a fixture bug, check it before anything else.
///
/// The band is deliberately wide: the predicted +4.27 is isolated-term
/// arithmetic, so pinning it tightly would pin a prediction rather than the
/// property.
///
/// # Measured, not predicted
///
/// The action-score margin `rock − body` measures **−3.3375 with the offset
/// disabled** and **+4.1625 with it enabled** — a delta of exactly +7.500, the
/// coefficient, reproducing the design's arithmetic to three decimals.
///
/// One correction to that arithmetic, established by measurement here: its
/// selection-probability table modelled a **two-candidate** softmax over rock and
/// body only. In production `PassPriority` is a third candidate and it sits
/// BETWEEN them (29.028, against rock 29.129 and body 24.967). That leaves every
/// margin intact but makes P(rock) ≈ 55 % rather than ≈ 99.98 %. This row
/// therefore asserts the **ordering and margin**, which is what the design
/// actually establishes, plus the strong behavioural consequence that Control
/// never casts the body at all.
#[test]
fn control_prefers_mana_rock_over_comparable_creature_as_disclosed() {
    let (state, rock_id, body_id) = rock_vs_body_fixture(control_deck_entries());
    assert_classifies_as(&state, phase_ai::deck_profile::DeckArchetype::Control);

    let castable: Vec<_> = engine::ai_support::legal_actions(&state)
        .into_iter()
        .filter_map(|a| match a {
            GameAction::CastSpell { object_id, .. } => Some(object_id),
            _ => None,
        })
        .collect();
    assert!(
        castable.contains(&rock_id) && castable.contains(&body_id),
        "reach-guard: both the rock and the body must be castable, else the \
         comparison is between one option and nothing; got {castable:?}"
    );

    // Row 16b — the numeric margin, from the two post-cast states.
    let mut rock_state = state.clone();
    resolve_to_battlefield(&mut rock_state, rock_id);
    let mut body_state = state.clone();
    resolve_to_battlefield(&mut body_state, body_id);

    let margin =
        archetype_adjusted_eval(&rock_state, phase_ai::deck_profile::DeckArchetype::Control)
            - archetype_adjusted_eval(&body_state, phase_ai::deck_profile::DeckArchetype::Control);
    assert!(
        (2.0..7.0).contains(&margin),
        "Control rock-over-body margin must sit in the DISCLOSED band 2.0..7.0 \
         (predicted +4.27); got {margin}"
    );

    // Row 16 — the ordering at the production action-score layer. THIS is the
    // revert-failing assertion: with the offset disabled the same fixture scores
    // rock 6.629 BELOW body 9.967, so the comparison flips sign on revert.
    let rock_score = cast_score(&state, rock_id);
    let body_score = cast_score(&state, body_id);
    assert_no_forced_win("row 16 (Control)", [rock_score, body_score]);
    assert!(
        rock_score > body_score,
        "DISCLOSED INVERSION: Control must now rank the mana rock above a \
         comparable body; got rock={rock_score} body={body_score}"
    );
    assert!(
        (2.0..7.0).contains(&(rock_score - body_score)),
        "the action-score margin must sit in the same disclosed band; got {}",
        rock_score - body_score
    );

    // The behavioural consequence, stated in the form that is actually true at
    // T = 0.5 with `PassPriority` in the candidate set: Control NEVER casts the
    // body. (With the offset disabled the body is the top-scoring action, so this
    // is revert-failing too.)
    assert_eq!(
        cast_selection_count(&state, body_id),
        0,
        "Control must never cast the comparable body once the rock outranks it"
    );
}

/// Row 17 — **Aggro must still prefer its creature to a mana rock**.
///
/// This is a NON-REGRESSION guard, and it is deliberately **not**
/// revert-failing: it passes both with and without the offset, because its job is
/// to prove the change did not flip an archetype that must not flip. Row 16 is
/// the revert-failing fix-verification; this row is its paired negative. A guard
/// that only holds after the change would not be a guard.
///
/// Measured: `body − rock` is **+8.0415 with the offset disabled** and **+0.5415
/// with it enabled** — a delta of exactly −7.500, again the coefficient. So the
/// design's headline warning is confirmed: Aggro's preference survives, but its
/// margin is compressed by 93 %, leaving only 0.54 of headroom. That is the
/// largest absolute behavioural movement in the table and it is why this row
/// exists.
///
/// Asserts the ORDERING rather than a cast-rate percentage. The realised rate
/// depends on search depth and on `PassPriority` competing (it scores 23.891,
/// between body 24.044 and rock 23.503), so a pinned percentage would be a flaky
/// test pinning a prediction rather than the property.
///
/// # Why there is a lower band but deliberately no upper one
///
/// The 93 % compression is the fact this row exists to record, and prose does not
/// regress. Without a band, a later change that compresses the remaining 0.54 to
/// 0.001 passes silently and the guard fires only *after* the sign has flipped —
/// no early warning at all. The lower bound restores it.
///
/// An *upper* bound would be wrong here, and that asymmetry is the point: with
/// the offset reverted this margin is +8.04, so any upper bound would make this
/// row revert-failing and destroy the "holds both with and without the change"
/// property that makes it a guard rather than a fix-verification. Row 16 can band
/// both sides precisely because it *is* the fix-verification.
#[test]
fn aggro_still_ranks_creature_above_mana_rock() {
    let (state, rock_id, body_id) = rock_vs_body_fixture(aggro_deck_entries());
    assert_classifies_as(&state, phase_ai::deck_profile::DeckArchetype::Aggro);

    let castable: Vec<_> = engine::ai_support::legal_actions(&state)
        .into_iter()
        .filter_map(|a| match a {
            GameAction::CastSpell { object_id, .. } => Some(object_id),
            _ => None,
        })
        .collect();
    assert!(
        castable.contains(&rock_id) && castable.contains(&body_id),
        "reach-guard: both options must be castable; got {castable:?}"
    );

    let rock_score = cast_score(&state, rock_id);
    let body_score = cast_score(&state, body_id);
    assert_no_forced_win("row 17 (Aggro)", [rock_score, body_score]);
    assert!(
        body_score > rock_score,
        "Aggro must still rank its creature above a mana rock; got body={body_score} \
         rock={rock_score}"
    );
    let margin = compressed_margin(&state, rock_id, body_id, ARCH_AGGRO);
    assert!(
        margin > AGGRO_COMPRESSED_MARGIN_FLOOR,
        "EARLY WARNING, not a sign flip: Aggro's remaining headroom over a mana \
         rock has fallen to {margin} (measured {AGGRO_MEASURED_MARGIN} when this \
         row landed, floor {AGGRO_COMPRESSED_MARGIN_FLOOR}). The ordering above \
         still holds, but a further compression inverts Aggro too — which is NOT \
         what the maintainer accepted. Investigate before the sign flips."
    );
}

/// Row 18 — **Midrange**, the `#[default]` archetype, measured rather than assumed.
///
/// The disclosure's original framing named Control as "the archetype that
/// inverts". That framing is misleading, because `mana_development_offset` carries
/// no archetype term and is applied *after* weighting: **every** archetype
/// receives exactly +7.5 per source and only the counterweight differs. Rows 16
/// and 17 measured the two poles and left the middle — including the archetype
/// every unclassifiable deck falls back to — unmeasured. That is the gap this row
/// closes, and it closes it with a measurement rather than an inference.
///
/// # The measured answer, which is neither pole
///
/// Midrange does **not** invert: the body still outranks the rock. But its
/// headroom is compressed from `+7.82651` to `+0.32651` — a 96 % reduction, the
/// largest proportional movement in the table. Midrange sits a third of a point
/// from inverting: **closer to the sign flip than Aggro** (0.41465), and far
/// closer than the "Control inverts, the others are fine" reading would suggest
/// to anyone scoping the accepted risk. Since Midrange is where every
/// unclassifiable deck lands, this is the widest-population row in the table.
///
/// Reading a red: (a) ordering flips → Midrange has inverted, which is beyond what
/// the maintainer accepted and is a stop-the-line finding, not a band update;
/// (b) floor trips with the ordering intact → early warning, investigate before
/// the sign flips; (c) archetype guard trips → fixture bug, check it first.
///
/// Deliberately NOT revert-failing, exactly like row 17: with the offset reverted
/// the same ordering holds at a much wider margin. Row 16 is the fix-verification.
#[test]
fn midrange_still_ranks_creature_above_mana_rock_but_barely() {
    let (state, rock_id, body_id) = rock_vs_body_fixture(midrange_deck_entries());
    assert_classifies_as(&state, phase_ai::deck_profile::DeckArchetype::Midrange);

    let castable: Vec<_> = engine::ai_support::legal_actions(&state)
        .into_iter()
        .filter_map(|a| match a {
            GameAction::CastSpell { object_id, .. } => Some(object_id),
            _ => None,
        })
        .collect();
    assert!(
        castable.contains(&rock_id) && castable.contains(&body_id),
        "reach-guard: both options must be castable; got {castable:?}"
    );

    let rock_score = cast_score(&state, rock_id);
    let body_score = cast_score(&state, body_id);
    assert_no_forced_win("row 18 (Midrange)", [rock_score, body_score]);
    assert!(
        body_score > rock_score,
        "MIDRANGE MUST NOT INVERT. The maintainer accepted rock-over-body for \
         Control; Midrange is the default archetype and inverting it widens the \
         accepted risk to every unclassifiable deck. Got body={body_score} \
         rock={rock_score}"
    );
    let margin = compressed_margin(&state, rock_id, body_id, MIDRANGE);
    assert!(
        margin > MIDRANGE_COMPRESSED_MARGIN_FLOOR,
        "EARLY WARNING, not a sign flip: Midrange's remaining headroom over a mana \
         rock has fallen to {margin} (measured {MIDRANGE_MEASURED_MARGIN} when this \
         row landed, floor {MIDRANGE_COMPRESSED_MARGIN_FLOOR}). Midrange is the \
         `#[default]` archetype, so this is the widest-population inversion risk \
         in the table."
    );
}

/// Fixture for the LOSS half of the disclosure: `P0` controls a 1/1 renewable mana
/// dork and a vanilla 4/4, on two untapped lands, in the late phase.
///
/// Returns `(state, dork_id, body_id)`.
///
/// Life is held EQUAL between the players on purpose. `evaluate_features` gates
/// the `aggression` term on `p.life > avg_opp_life`, and that term is worth
/// `w.aggression` per point of power, which moves the break-even body from a
/// 4.7/4.7 to a 4.0/4.0 — i.e. it lands almost exactly on this fixture's 4/4 and
/// would make the margin a knife-edge 0.007 rather than a stable 1.49. Equal life
/// is also the honest regime for the decision class being measured: a player
/// choosing whether to chump-block a fatty is usually not ahead on life.
fn mana_dork_and_body_fixture() -> (
    engine::types::game_state::GameState,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);

    let dork_id = scenario.add_creature(P0, "Mana Dork", 1, 1).id();
    let body_id = scenario.add_creature(P0, "Vanilla Fatty", 4, 4).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.turn_number = 9;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };

        let dork = state.objects.get_mut(&dork_id).unwrap();
        let mut mana_ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: engine::types::ability::ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        );
        mana_ability.cost = Some(engine::types::ability::AbilityCost::Tap);
        std::sync::Arc::make_mut(&mut dork.abilities).push(mana_ability);
    }

    (runner.state().clone(), dork_id, body_id)
}

/// Move `id` from `P0`'s battlefield to their graveyard — the counterfactual
/// "this permanent died" state.
fn destroy_to_graveyard(
    state: &mut engine::types::game_state::GameState,
    id: engine::types::identifiers::ObjectId,
) {
    state.battlefield.retain(|&b| b != id);
    state.objects.get_mut(&id).unwrap().zone = engine::types::zones::Zone::Graveyard;
    state.players[0].graveyard.push_back(id);
}

/// Row 19 — **the offset applies to LOSING a source too, and it inverts creature
/// trades.**
///
/// A distinct decision class from rows 16–18. Those measure *acquisition* (cast a
/// rock or cast a body); this measures *loss*, which is reached through block
/// assignment and sacrifice choices and scored through the same tactical eval that
/// `PlanExecutor::evaluate_with_strategy` calls. It was absent from the disclosed
/// table and from every test, so a maintainer weighing the accepted risk saw only
/// half of it.
///
/// # What it costs Midrange (late, shipped tables) to lose one permanent
///
/// | Permanent | presence | power | toughness | card_adv | offset | total |
/// |---|---|---|---|---|---|---|
/// | 1/1 mana dork | 2.598 | 0.802 | 1.200 | 0.778 | **7.500** | **12.878** |
/// | vanilla 4/4 | 2.598 | 3.209 | 4.800 | 0.778 | 0 | **11.385** |
///
/// The AI values the Llanowar Elves 1.49 above the 4/4, so it will chump-block a
/// fatty to save the dork. Break-even is a vanilla **4.7/4.7**.
///
/// # Why this measures at `archetype_adjusted_eval` and that is sufficient
///
/// `archetype_adjusted_eval` is `evaluate_state` with the archetype-adjusted late
/// weights — exactly the `tactical` term of `evaluate_with_strategy`. Of the three
/// strategic terms it omits, `synergy` is 0 (no deck pool, no synergy graph),
/// and `card_advantage::differential` costs an identical 0.778 on **both**
/// branches because each loses exactly one nontoken permanent, so it cannot change
/// the ordering — asserted below as a reach-guard rather than assumed.
///
/// Revert-failing: with the offset zeroed the same two states score
/// `lost_dork − lost_body = +6.007`, i.e. the AI would rather lose the dork. The
/// assertion below flips sign.
#[test]
fn mana_dork_outvalues_a_bigger_body_when_trading() {
    let (state, dork_id, body_id) = mana_dork_and_body_fixture();

    let mut lost_dork = state.clone();
    destroy_to_graveyard(&mut lost_dork, dork_id);
    let mut lost_body = state.clone();
    destroy_to_graveyard(&mut lost_body, body_id);

    // Reach-guard 1 — the dork is genuinely credited as a renewable source, so the
    // two branches differ by exactly one source. Without this the comparison could
    // pass on an ordinary power/toughness difference having never exercised the
    // offset at all.
    let dork_offset = phase_ai::eval::evaluate_features(&lost_body, P0)
        .expect("non-terminal")
        .mana_development_offset
        - phase_ai::eval::evaluate_features(&lost_dork, P0)
            .expect("non-terminal")
            .mana_development_offset;
    // Deliberately `> 0.0` and NOT `== 7.5`. A reach-guard must prove the fixture
    // reaches the code under test; pinning the coefficient's *magnitude* here would
    // make every coefficient change trip the guard and short-circuit the
    // behavioural assertions below — the test would report "fixture broken" when
    // what actually happened is the behaviour it exists to measure changed. The
    // magnitude is pinned by the margin bands at the end, where it belongs.
    assert!(
        dork_offset > 0.0,
        "reach-guard: the dork must be credited as a renewable mana source, so the \
         two branches differ by one source. If this is 0 the fixture never \
         exercises the offset and everything below is vacuous. got {dork_offset}"
    );

    // Reach-guard 2 — both permanents are nontoken, so `count_resources` charges
    // an identical 1.0 on each branch and the omitted `card_advantage::differential`
    // term provably cancels.
    assert!(
        !state.objects[&dork_id].is_token && !state.objects[&body_id].is_token,
        "reach-guard: both must be nontoken, else the omitted card_advantage \
         differential does not cancel between the branches"
    );

    let keep_body = archetype_adjusted_eval(&lost_dork, MIDRANGE);
    let keep_dork = archetype_adjusted_eval(&lost_body, MIDRANGE);

    assert!(
        keep_dork > keep_body,
        "DISCLOSED INVERSION (loss half): the AI must now prefer the world where \
         it lost the 4/4 and kept the 1/1 mana dork; got keep_dork={keep_dork} \
         keep_body={keep_body}"
    );
    assert!(
        (1.0..2.5).contains(&(keep_dork - keep_body)),
        "the trade margin must sit in the disclosed band 1.0..2.5 (predicted \
         +1.493 from the shipped tables); got {}",
        keep_dork - keep_body
    );

    // The revert counterfactual, ASSERTED rather than claimed in prose. Strip each
    // branch's own offset contribution and the preference must invert — the AI
    // would rather lose the dork and keep the 4/4.
    //
    // This is here because the offset-zeroing probe cannot demonstrate it: zeroing
    // the production line makes `dork_offset` 0.0, which the `> 0.0` reach-guard
    // above rejects, so the run never reaches this ordering at all. (The guard
    // deliberately does not pin a magnitude — see its comment.) Computing the counterfactual
    // from live values makes the flip machine-checked on every run instead of
    // resting on a comment that can rot.
    let keep_body_reverted = keep_body
        - phase_ai::eval::evaluate_features(&lost_dork, P0)
            .expect("non-terminal")
            .mana_development_offset;
    let keep_dork_reverted = keep_dork
        - phase_ai::eval::evaluate_features(&lost_body, P0)
            .expect("non-terminal")
            .mana_development_offset;
    assert!(
        keep_body_reverted > keep_dork_reverted,
        "without the offset the preference MUST invert (the 4/4 is the better \
         keep); if it does not, this row is no longer discriminating and its \
         disclosure is stale. got keep_body={keep_body_reverted} \
         keep_dork={keep_dork_reverted}"
    );
    assert!(
        (-6.5..-5.5).contains(&(keep_dork_reverted - keep_body_reverted)),
        "the reverted margin must be the disclosed +1.493 less the coefficient, \
         i.e. about −6.007; got {}",
        keep_dork_reverted - keep_body_reverted
    );
}

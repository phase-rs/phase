//! CR 601.2h + CR 115.10a — the AI answering Invigorate's cast-time recipient
//! prompt ("you may have an opponent gain 3 life") with two choosable opponents.
//!
//! No AI policy exists for this prompt on purpose: the candidates come from the
//! engine (`WaitingFor::ChooseGiftRecipient` with
//! `CastOpponentChoicePurpose::EffectCost`) and search simulates each one under
//! the threat-weighted multiplayer eval. P1 controls two 5/5s and P2 controls
//! nothing, so handing the life to P2 (the less threatening opponent) is the
//! sensible answer.
//!
//! Hard and VeryHard pick the top-ranked candidate, so they must pick P2 on
//! every seed. Medium samples from a softmax over the ranked candidates by
//! design, so its pick may vary by seed; there the test asserts the RANKING (P2
//! is the top-ranked candidate in the diagnostic receipt), and that whatever it
//! samples is a legal candidate that completes the cast.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastOpponentChoicePurpose, CastPaymentMode, GameState, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use phase_ai::config::{create_config, AiDifficulty, Platform};
use phase_ai::eval::threat_level;
use phase_ai::search::{choose_action, choose_action_with_session_diagnostic};
use phase_ai::session::AiSession;
use rand::rngs::SmallRng;
use rand::SeedableRng;

const P2: PlayerId = PlayerId(2);
const SEEDS: std::ops::Range<u64> = 0..10;

/// Invigorate's Oracle text, verbatim from the local Scryfall-derived dataset.
const INVIGORATE: &str = "If you control a Forest, rather than pay this spell's mana cost, you may have an opponent gain 3 life.\nTarget creature gets +4/+4 until end of turn.";

/// Three players at P0's recipient prompt. Returns the state and Invigorate's id.
fn state_at_recipient_prompt() -> (GameState, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario.add_creature(P1, "Big One", 5, 5);
    scenario.add_creature(P1, "Big Two", 5, 5);
    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, "Invigorate", true, INVIGORATE);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 2,
        shards: vec![ManaCostShard::Green],
    });
    let inv = builder.id();
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&inv].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: inv,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should start");
    for _ in 0..8 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(bear)],
                    })
                    .expect("target");
            }
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("accept the alternative cost");
            }
            WaitingFor::ChooseGiftRecipient {
                player,
                candidates,
                purpose,
                ..
            } => {
                assert_eq!(player, P0);
                assert_eq!(candidates, vec![P1, P2]);
                assert!(matches!(
                    purpose,
                    CastOpponentChoicePurpose::EffectCost { .. }
                ));
                return (runner.state().clone(), inv);
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("recipient prompt never raised");
}

/// Apply `action` to a fresh copy of `state` and check the cast completes: P0
/// gets priority back with Invigorate on the stack and the chosen opponent +3.
fn assert_answer_completes_cast(state: &GameState, inv: ObjectId, action: &GameAction) {
    let GameAction::ChooseGiftRecipient { opponent } = *action else {
        panic!("not a recipient answer: {action:?}");
    };
    assert!(
        legal_actions(state).contains(action),
        "{action:?} is a legal candidate"
    );
    let mut runner = GameRunner::from_state(state.clone());
    let before = runner.life(opponent);
    runner.act(action.clone()).expect("answer applies");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.iter().any(|e| e.id == inv));
    assert_eq!(runner.life(opponent), before + 3);
}

#[test]
fn ai_gives_the_life_to_the_less_threatening_opponent() {
    let (state, inv) = state_at_recipient_prompt();
    // Setup anti-vacuity: the two opponents really differ in threat.
    assert!(
        threat_level(&state, P0, P1) > threat_level(&state, P0, P2),
        "P1 (two 5/5s) must be the bigger threat"
    );
    let expected = GameAction::ChooseGiftRecipient { opponent: P2 };

    for difficulty in [AiDifficulty::Hard, AiDifficulty::VeryHard] {
        let config = create_config(difficulty, Platform::Native);
        for seed in SEEDS {
            let action = choose_action(&state, P0, &config, &mut SmallRng::seed_from_u64(seed));
            assert_eq!(
                action.as_ref(),
                Some(&expected),
                "{difficulty:?} seed {seed}"
            );
            assert_answer_completes_cast(&state, inv, &expected);
        }
    }

    // Medium samples by design: assert the ranking, and that the sample is legal
    // and completes the cast.
    let config = create_config(AiDifficulty::Medium, Platform::Native);
    let session = AiSession::arc_from_game(&state);
    for seed in SEEDS {
        let selection = choose_action_with_session_diagnostic(
            &state,
            P0,
            &config,
            &mut SmallRng::seed_from_u64(seed),
            &session,
        );
        let receipt = selection.receipt.expect("diagnostic receipt");
        let top: Vec<&GameAction> = receipt
            .candidates
            .iter()
            .filter(|c| c.is_top_ranked)
            .map(|c| &c.action)
            .collect();
        assert_eq!(top, vec![&expected], "Medium seed {seed}: top-ranked");
        let action = selection.action.expect("Medium answers the prompt");
        assert_answer_completes_cast(&state, inv, &action);
    }
}

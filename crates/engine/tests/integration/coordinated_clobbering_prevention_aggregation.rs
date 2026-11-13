//! Blocker-1 regression: targeted-source multi-damage resolves through the
//! shared owned noncombat batch (CR 615.13), so simultaneous prevention is ONE
//! application per (shield, recipient) — even across a replacement pause.
//!
//! Coordinated Clobbering makes two 1-power sources each deal 1 to Selfless
//! Squire; three Jade Monolith activations (two naming the first source, one
//! naming the second) redirect both hits to the Squire's controller, whose ETB
//! shield prevents them. Correct: one prevention of two, one Squire trigger,
//! 2 + 1 = 3 counters with Hardened Scales. The pre-batch path installed no
//! tally: the answered member's prevention resolved inside the answer path
//! (which never publishes per-event) while only the resumed member's
//! published — one lone application of one, 1 + 1 = 2 counters.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::zones::Zone;

// Verbatim Oracle texts (Scryfall named-card API; Scales/Monolith already pinned
// by `appended_token_batch_replacement_order` / `latched_redirect_controller`).
const COORDINATED_CLOBBERING: &str = "Tap one or two target untapped creatures you control. They each deal damage equal to their power to target creature an opponent controls.";
const SELFLESS_SQUIRE_TEXT: &str = "Flash\nWhen this creature enters, prevent all damage that would be dealt to you this turn.\nWhenever damage that would be dealt to you is prevented, put that many +1/+1 counters on this creature.";
const HARDENED_SCALES: &str = "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.";
const JADE: &str = "{1}: The next time a source of your choice would deal damage to target creature this turn, that source deals that damage to you instead.";

fn add_mana(runner: &mut GameRunner, player: PlayerId, color: ManaType, count: usize) {
    for _ in 0..count {
        runner
            .state_mut()
            .add_mana_to_pool(player, ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
}

fn priority(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..runner.state().players.len() {
        match runner.state().waiting_for {
            WaitingFor::Priority { player: current } if current == player => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            ref other => panic!("expected priority, got {other:?}"),
        }
    }
    panic!("priority did not reach {player:?}");
}

fn life_of(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .life
}

fn counters_on(runner: &GameRunner, obj: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&obj)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

/// Activate one Monolith naming `source`, shielding `protected` (mirrors
/// `latched_redirect_controller`).
fn install_monolith_redirect(
    runner: &mut GameRunner,
    jade: ObjectId,
    protected: ObjectId,
    source: ObjectId,
) {
    priority(runner, P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: jade,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("announce the Monolith activation");
    let WaitingFor::TargetSelection { .. } = runner.state().waiting_for else {
        panic!(
            "expected Monolith target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(protected)),
        })
        .expect("target the Squire");
    for _ in 0..16 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DamageSourceChoice { .. }
        ) {
            break;
        }
        assert!(
            !runner.state().stack.is_empty(),
            "Monolith must reach its source choice"
        );
        runner
            .act(GameAction::PassPriority)
            .expect("resolve Monolith through priority passes");
    }
    let WaitingFor::DamageSourceChoice {
        player, options, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected Monolith source choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert!(
        options.contains(&source),
        "the Clobbering source must be choosable"
    );
    runner
        .act(GameAction::ChooseDamageSource { source })
        .expect("resume Monolith with the selected source");
    assert!(runner.state().stack.is_empty());
}

#[test]
fn clobbering_redirected_twice_prevents_once_through_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let squire = scenario
        .add_creature_to_hand_from_oracle(P0, "Selfless Squire", 1, 1, SELFLESS_SQUIRE_TEXT)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
    let jades: Vec<ObjectId> = (0..3)
        .map(|_| {
            scenario
                .add_artifact_from_oracle(P0, "Jade Monolith", JADE)
                .id()
        })
        .collect();
    let src1 = scenario.add_vanilla(P1, 1, 1);
    let src2 = scenario.add_vanilla(P1, 1, 1);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Coordinated Clobbering", false, COORDINATED_CLOBBERING)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let p0_life = life_of(&runner, P0);

    // P0 setup: cast the Squire (its ETB shield must resolve) and install
    // three redirects — two naming src1 (the pair that forces the ordering
    // pause) and one naming src2.
    runner.cast(squire).resolve();
    assert_eq!(
        runner.state().objects.get(&squire).unwrap().zone,
        Zone::Battlefield,
        "the Squire must resolve so its ETB shield exists"
    );
    add_mana(&mut runner, P0, ManaType::Colorless, 3);
    install_monolith_redirect(&mut runner, jades[0], squire, src1);
    install_monolith_redirect(&mut runner, jades[1], squire, src1);
    install_monolith_redirect(&mut runner, jades[2], squire, src2);
    assert_eq!(
        runner
            .state()
            .objects
            .get(&squire)
            .unwrap()
            .replacement_definitions
            .len(),
        3,
        "all three redirects must be installed before the Clobbering"
    );

    // P1's turn, same turn number: every "this turn" shield is still alive.
    runner.state_mut().active_player = P1;
    priority(&mut runner, P1);
    let _ = runner
        .cast(spell)
        .target_objects(&[src1, src2, squire])
        .resolve();

    // The batch must pause for the src1 ordering choice — sailing through
    // would skip the parked-aggregate path this test exists for.
    let WaitingFor::ReplacementChoice { player, .. } = runner.state().waiting_for else {
        panic!(
            "expected the batch ReplacementChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player, P0,
        "CR 616.1: the Squire's controller orders its replacements"
    );
    let pending = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("a pending replacement backs the prompt");
    let ProposedEvent::Damage { source_id, .. } = &pending.proposed else {
        panic!(
            "expected the paused damage event, got {:?}",
            pending.proposed
        );
    };
    assert_eq!(*source_id, src1, "member 0 pauses; member 1 parks raw");
    assert_eq!(
        pending.candidates.len(),
        2,
        "both src1 redirects must compete (ordering, not optional)"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("order the first redirect first");
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the cast fully resolves, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    // Both sources tapped by the tap sentence (the spell fully executed).
    assert!(runner.state().objects.get(&src1).unwrap().tapped);
    assert!(runner.state().objects.get(&src2).unwrap().tapped);
    // Reach guard: the damage really was PREVENTED (not merely never dealt) —
    // and the Squire itself took nothing (everything redirected away).
    assert_eq!(life_of(&runner, P0), p0_life);
    assert_eq!(
        runner.state().objects.get(&squire).unwrap().zone,
        Zone::Battlefield
    );
    assert_eq!(
        runner.state().objects.get(&squire).unwrap().damage_marked,
        0
    );
    // CR 615.13: ONE application of two → one trigger → two counters →
    // Hardened Scales makes three. The pre-batch path yields two (only the
    // resumed member's prevention ever publishes).
    assert_eq!(counters_on(&runner, squire), 3);
}

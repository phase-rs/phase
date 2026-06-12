//! Issue #2351 — Aven Interrupter ETB must exile the targeted stack spell, not itself.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const AVEN_INTERRUPTER: &str = "Flash\nFlying\nWhen this creature enters, exile target spell. It becomes plotted. (Its owner may cast it as a sorcery on a later turn without paying its mana cost.)\nSpells your opponents cast from graveyards or from exile cost {2} more to cast.";

fn floating_mana(generic: usize, white: usize) -> Vec<ManaUnit> {
    let mut pool = Vec::new();
    for _ in 0..generic {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    for _ in 0..white {
        pool.push(ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]));
    }
    pool
}

fn resolve_targeting_and_stack(runner: &mut engine::game::scenario::GameRunner, target: TargetRef) {
    let target = Some(target);
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: target.clone(),
                    })
                    .expect("choose target");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.pass_both_players();
            }
            _ => break,
        }
    }
    runner.advance_until_stack_empty();
}

#[test]
fn issue_2351_aven_interrupter_exiles_target_spell_not_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let aven_hand = scenario
        .add_creature_to_hand_from_oracle(P0, "Aven Interrupter", 2, 2, AVEN_INTERRUPTER)
        .id();
    let opponent_spell = scenario.add_bolt_to_hand(P1);
    scenario.with_mana_pool(P0, floating_mana(1, 2));
    scenario.with_mana_pool(P1, floating_mana(1, 0));

    let mut runner = scenario.build();

    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    // Opponent casts Lightning Bolt targeting P0; leave it on the stack.
    let bolt_card_id = runner.state().objects[&opponent_spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: opponent_spell,
            card_id: bolt_card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 casts Lightning Bolt");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P0)),
            })
            .expect("Lightning Bolt targets P0");
    }

    // P1 passes so P0 can cast Aven Interrupter with Flash in response.
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority to P0");

    assert!(
        runner.state().stack.iter().any(|e| e.id == opponent_spell),
        "Lightning Bolt should remain on the stack before Aven is cast, got stack {:?}",
        runner
            .state()
            .stack
            .iter()
            .map(|e| e.id)
            .collect::<Vec<_>>()
    );

    let shock_on_stack = opponent_spell;
    let aven_card_id = runner.state().objects[&aven_hand].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: aven_hand,
            card_id: aven_card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts Aven Interrupter with Flash");

    resolve_targeting_and_stack(&mut runner, TargetRef::Object(shock_on_stack));

    let state = runner.state();
    assert_eq!(
        state.objects[&aven_hand].zone,
        Zone::Battlefield,
        "Aven Interrupter must remain on the battlefield"
    );
    assert_eq!(
        state.objects[&shock_on_stack].zone,
        Zone::Exile,
        "Aven Interrupter must exile the targeted spell, not itself"
    );
    assert!(
        state
            .objects
            .get(&aven_hand)
            .is_some_and(|obj| obj.zone != Zone::Exile),
        "Aven Interrupter must not exile itself"
    );
}

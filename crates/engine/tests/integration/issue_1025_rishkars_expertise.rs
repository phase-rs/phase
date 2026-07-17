//! Issue #1025 — Rishkar's Expertise must cast the free spell during resolution
//! instead of stalling in a non-actionable waiting state after the hand pick.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{CastingPermission, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const RISHKAR_EXPERTISE: &str =
    "Draw cards equal to the greatest power among creatures you control. \
You may cast a spell with mana value 5 or less from your hand without paying its mana cost.";

fn floating_mana(generic: usize, green: usize) -> Vec<ManaUnit> {
    let mut pool = Vec::new();
    for _ in 0..generic {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    for _ in 0..green {
        pool.push(ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]));
    }
    pool
}

#[test]
fn rishkars_expertise_free_cast_completes_during_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.with_library_top(
        P0,
        &[
            "Library One",
            "Library Two",
            "Library Three",
            "Library Four",
            "Library Five",
        ],
    );
    scenario.add_creature(P0, "Elf", 1, 1);
    let expertise = scenario
        .add_spell_to_hand_from_oracle(P0, "Rishkar's Expertise", false, RISHKAR_EXPERTISE)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
        })
        .id();
    let free_spell = scenario
        .add_spell_to_hand(P0, "Free Bolt", true)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("Draw a card.")
        .id();

    scenario.with_mana_pool(P0, floating_mana(4, 2));

    let mut runner = scenario.build();
    let expertise_card_id = runner.state().objects[&expertise].card_id;

    let parsed = &runner.state().objects[&expertise].abilities[0];
    let cast = parsed.sub_ability.as_ref().expect("free cast sub-ability");
    assert!(matches!(
        cast.effect.as_ref(),
        Effect::CastFromZone {
            without_paying_mana_cost: true,
            ..
        }
    ));

    runner
        .act(GameAction::CastSpell {
            object_id: expertise,
            card_id: expertise_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Rishkar's Expertise");

    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "expected optional free-cast prompt, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept free cast");

    match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice { cards, .. } => {
            assert!(cards.contains(&free_spell));
        }
        other => panic!("expected hand pick for free cast, got {other:?}"),
    }

    runner
        .act(GameAction::SelectCards {
            cards: vec![free_spell],
        })
        .expect("pick free spell");

    assert_eq!(
        runner.state().objects[&free_spell].zone,
        Zone::Stack,
        "free spell must reach the stack during resolution"
    );
    assert!(
        matches!(
            runner.state().objects[&free_spell]
                .casting_permissions
                .as_slice(),
            [CastingPermission::ExileWithAltCost {
                resolution_cleanup: None,
                mana_spend_permission: None,
                graveyard_replacement: None,
                enters_with_counter: None,
                enters_with_modifications,
                ..
            }] if enters_with_modifications.is_empty()
        ),
        "the consumed free-cast permission must remain only as a neutral stable slot"
    );

    runner.advance_until_stack_empty();

    assert!(
        runner.state().objects[&free_spell]
            .casting_permissions
            .is_empty(),
        "normal Stack exit cleanup must remove the neutral consumed slot"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "game must return to actionable priority, got {:?}",
        runner.state().waiting_for
    );
}

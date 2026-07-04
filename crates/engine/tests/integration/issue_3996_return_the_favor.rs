//! Regression for issue #3996: Return the Favor's copy mode must target instants,
//! sorceries, and stack abilities — not only instants.
//!
//! https://github.com/phase-rs/phase/issues/3996

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetFilter, TypeFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const RETURN_THE_FAVOR_ORACLE: &str = "Spree (Choose one or more additional costs.)\n\
+ {1} — Copy target instant spell, sorcery spell, activated ability, or triggered ability. You may choose new targets for the copy.\n\
+ {1} — Change the target of target spell or ability with a single target.";

fn put_sorcery_on_stack(
    runner: &mut engine::game::scenario::GameRunner,
    controller: engine::types::player::PlayerId,
) -> ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(601),
        controller,
        "Sorcery Fodder".to_string(),
        Zone::Stack,
    );
    if let Some(obj) = runner.state_mut().objects.get_mut(&spell) {
        obj.card_types.core_types = vec![CoreType::Sorcery];
    }
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(601),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

#[test]
fn return_the_favor_copy_mode_parses_four_way_stack_object_filter() {
    let mut scenario = GameScenario::new();
    let favor = scenario
        .add_spell_to_hand_from_oracle(P0, "Return the Favor", true, RETURN_THE_FAVOR_ORACLE)
        .id();
    let runner = scenario.build();
    let ability = &runner.state().objects[&favor].abilities[0];
    let Effect::CopySpell { target, .. } = ability.effect.as_ref() else {
        panic!("mode 1 must parse to CopySpell, got {:?}", ability.effect);
    };
    let TargetFilter::Or { filters } = target else {
        panic!("copy mode must use Or stack-object filter, got {target:?}");
    };
    assert!(
        filters.iter().any(|f| {
            matches!(
                f,
                TargetFilter::Typed(tf) if tf.type_filters == [TypeFilter::Sorcery]
            )
        }),
        "sorcery spell leg required: {filters:?}"
    );
}

#[test]
fn return_the_favor_copy_mode_available_against_sorcery_on_stack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let favor = scenario
        .add_spell_to_hand_from_oracle(P0, "Return the Favor", true, RETURN_THE_FAVOR_ORACLE)
        .id();
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Red);

    let mut runner = scenario.build();
    let _opponent_sorcery = put_sorcery_on_stack(&mut runner, P1);

    let card_id = runner.state().objects[&favor].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: favor,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Return the Favor");

    for _ in 0..24 {
        let waiting = runner.state().waiting_for.clone();
        match waiting {
            WaitingFor::ModeChoice {
                unavailable_modes, ..
            } => {
                assert!(
                    !unavailable_modes.contains(&0),
                    "copy mode must be legal when a sorcery is on the stack, \
                     unavailable={unavailable_modes:?}"
                );
                return;
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } => {
                let _ = runner.act(GameAction::PassPriority);
            }
            _ => {
                let _ = runner.act(GameAction::PassPriority);
            }
        }
    }
    panic!("cast pipeline never reached ModeChoice");
}

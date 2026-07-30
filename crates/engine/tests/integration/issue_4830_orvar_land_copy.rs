//! Issue #4830: Orvar must copy a targeted land you control, not Orvar itself.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORVAR: &str = "Changeling\n\
Whenever you cast an instant or sorcery spell, if it targets one or more other permanents you control, create a token that's a copy of one of those permanents.\n\
When a spell or ability an opponent controls causes you to discard this card, create a token that's a copy of target permanent.";

const AWAKENING: &str = "Put nine +1/+1 counters on target land you control. It becomes a legendary 0/0 Elemental creature with haste named Vitu-Ghazi. It's still a land.";

fn add_mana(runner: &mut GameRunner, n: usize, mana_type: ManaType) {
    for _ in 0..n {
        runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == P0)
            .unwrap()
            .mana_pool
            .add(ManaUnit::new(mana_type, ObjectId(0), false, vec![]));
    }
}

fn drive_cast_target(runner: &mut GameRunner, target: TargetRef) {
    for _ in 0..48 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(target.clone()),
                    })
                    .expect("choose cast target");
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let t = target_slots[selection.current_slot]
                    .legal_targets
                    .first()
                    .cloned();
                runner
                    .act(GameAction::ChooseTarget { target: t })
                    .expect("choose copy target");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
        }
    }
}

#[test]
fn orvar_copies_targeted_land_not_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let orvar = scenario
        .add_creature_from_oracle(P0, "Orvar, the All-Form", 3, 3, ORVAR)
        .id();
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Awakening of Vitu-Ghazi", true, AWAKENING)
        .id();

    let mut runner = scenario.build();
    add_mana(&mut runner, 3, ManaType::Colorless);
    add_mana(&mut runner, 2, ManaType::Green);

    let permanents_before = runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.controller == P0)
        .count();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast spell");

    drive_cast_target(&mut runner, TargetRef::Object(land));
    runner.advance_until_stack_empty();

    let battlefield: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.controller == P0)
        .collect();

    assert_eq!(
        battlefield.len(),
        permanents_before + 1,
        "Orvar must create exactly one token copy of the targeted land"
    );
    assert!(
        runner.state().objects.contains_key(&orvar),
        "Orvar itself must remain on the battlefield"
    );
    assert!(
        runner.state().objects[&land].zone == Zone::Battlefield,
        "the targeted land must remain on the battlefield"
    );

    let orvar_permanents = battlefield
        .iter()
        .filter(|o| o.name == "Orvar, the All-Form" && !o.is_token)
        .count();
    assert_eq!(
        orvar_permanents, 1,
        "must not create a second Orvar permanent"
    );

    let orvar_tokens = battlefield
        .iter()
        .filter(|o| o.is_token && o.name.contains("Orvar"))
        .count();
    assert_eq!(orvar_tokens, 0, "must not create a token copy of Orvar");

    let land_token_copies = battlefield
        .iter()
        .filter(|o| o.is_token && o.id != land && o.card_types.core_types.contains(&CoreType::Land))
        .count();
    assert_eq!(
        land_token_copies, 1,
        "exactly one land token copy must be created"
    );
}

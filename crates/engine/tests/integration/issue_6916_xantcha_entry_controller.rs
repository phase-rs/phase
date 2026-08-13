//! Xantcha regression (#6916): its mandatory self-entry controller choice is
//! made before battlefield delivery, not by seat-order fallback or post-entry
//! control-changing effect.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::PlayerId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const XANTCHA_ORACLE: &str = "Xantcha enters under the control of an opponent of your choice.\nXantcha attacks each combat if able and can't attack its owner or planeswalkers its owner controls.\n{3}: Xantcha's controller loses 2 life and you draw a card. Any player may activate this ability.";
const P2: PlayerId = PlayerId(2);

#[test]
fn xantcha_chooses_entry_controller_before_battlefield_delivery() {
    let mut scenario = GameScenario::new_n_player(3, 0x6916);
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..6 {
        scenario.add_basic_land(P1, ManaColor::Red);
    }
    scenario.with_library_top(
        P1,
        &["Xantcha Activation Draw One", "Xantcha Activation Draw Two"],
    );
    let xantcha = scenario
        .add_creature_to_hand_from_oracle(P0, "Xantcha, Sleeper Agent", 5, 5, XANTCHA_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(xantcha).resolve();
    let WaitingFor::EntryControllerChoice { player, candidates } = outcome.final_waiting_for() else {
        panic!("Xantcha must choose an entry controller, got {:?}", outcome.final_waiting_for());
    };
    assert_eq!(*player, P0);
    assert_eq!(candidates, &[P1, P2]);
    assert_eq!(
        runner.state().objects[&xantcha].zone,
        Zone::Stack,
        "the entrant must remain out of the battlefield until the choice resolves"
    );

    runner
        .act(GameAction::ChooseEntryController { opponent: P2 })
        .expect("the offered opponent is a legal entry controller");
    runner.advance_until_stack_empty();

    let xantcha_state = &runner.state().objects[&xantcha];
    assert_eq!(xantcha_state.zone, Zone::Battlefield);
    assert_eq!(xantcha_state.controller, P2);

    // CR 113.7a: P1 activates, but "Xantcha's controller" is the source's
    // current controller P2; the ordinary "you draw" still belongs to P1.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    let p1_hand_before = runner.state().players[P1.0 as usize].hand.len();
    let p2_life_before = runner.state().players[P2.0 as usize].life;
    runner
        .act(GameAction::ActivateAbility {
            source_id: xantcha,
            ability_index: 0,
        })
        .expect("P1 may activate Xantcha from priority");
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().players[P2.0 as usize].life, p2_life_before - 2);
    assert_eq!(
        runner.state().players[P1.0 as usize].hand.len(),
        p1_hand_before + 1,
        "the activation's ordinary controller-relative draw remains the activator"
    );

    // CR 608.2h + CR 113.7a: the activation's source leaves and returns under
    // a new incarnation before resolution. Its "~'s controller" reference
    // must use P2's exact activation-time LKI, not the same id's new P0
    // incarnation; the ordinary "you draw" remains the P1 activator.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    let p1_hand_before_lki = runner.state().players[P1.0 as usize].hand.len();
    let p2_life_before_lki = runner.state().players[P2.0 as usize].life;
    runner
        .act(GameAction::ActivateAbility {
            source_id: xantcha,
            ability_index: 0,
        })
        .expect("Xantcha activation must reach the stack before priority passes");

    let mut move_events = Vec::new();
    move_to_zone(runner.state_mut(), xantcha, Zone::Exile, &mut move_events);
    move_to_zone(
        runner.state_mut(),
        xantcha,
        Zone::Battlefield,
        &mut move_events,
    );
    let returned = runner
        .state_mut()
        .objects
        .get_mut(&xantcha)
        .expect("Xantcha returns as a new object");
    returned.base_controller = Some(P0);
    returned.controller = P0;
    assert_eq!(runner.state().objects[&xantcha].controller, P0);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P2.0 as usize].life,
        p2_life_before_lki - 2,
        "the activated source's departed P2 incarnation remains authoritative"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].hand.len(),
        p1_hand_before_lki + 1,
        "the second activation's ordinary controller-relative draw remains P1's"
    );
}

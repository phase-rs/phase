//! MSH Wave 1 — Klaw, Master of Sound: "Whenever you play a card from exile, …".
//!
//! `PlayCard` triggers now accept an optional `from <zone>` origin tail. Klaw is
//! not in the local fixture (MSH is release-gated), so this drives the real
//! parser + trigger matcher through representative Oracle text.
//!
//! "Play a card" = cast a spell OR play a land (CR 601.1a). The cast half routes
//! the origin through `spell_cast_origin` (already runtime-honored by
//! `match_spell_cast`, exercised by Rocco, Street Chef). This test pins the LAND
//! half: `match_play_card` gates `LandPlayed` events on the same
//! `spell_cast_origin` constraint (CR 305.1). One test exercises both the parser
//! (D.2 — capturing `from exile`) and the matcher gate (D.3):
//!   * Revert D.2 (origin stays `Any`) ⇒ the hand-play case fires ⇒ assertion fails.
//!   * Revert D.3 (no land-half gate)  ⇒ the hand-play case fires ⇒ assertion fails.

use engine::game::scenario::GameScenario;
use engine::game::triggers::process_triggers;
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const KLAW_ORACLE: &str = "Whenever you play a card from exile, you gain 2 life.";

fn drain_priority(runner: &mut engine::game::scenario::GameRunner) {
    let mut guard = 0;
    while !runner.state().stack.is_empty() {
        guard += 1;
        assert!(guard < 60, "stack did not drain");
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// Play a land FROM EXILE ⇒ Klaw fires (gain 2 life).
#[test]
fn klaw_fires_on_land_played_from_exile() {
    let mut scenario = GameScenario::new_n_player(2, 13);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Klaw, Master of Sound", 2, 3, KLAW_ORACLE);

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let land = create_object(
        runner.state_mut(),
        CardId(50_000),
        P0,
        "Some Land".to_string(),
        Zone::Battlefield,
    );

    let life_before = runner.life(P0);
    let events = vec![GameEvent::LandPlayed {
        object_id: land,
        player_id: P0,
        from_zone: Zone::Exile,
    }];
    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    assert_eq!(
        runner.life(P0),
        life_before + 2,
        "playing a land from exile must fire 'whenever you play a card from exile'"
    );
}

/// Play a land FROM HAND ⇒ Klaw does NOT fire. This is the discriminating
/// negative: without the parser origin capture (D.2) or the land-half origin
/// gate (D.3), this hand play would also fire.
#[test]
fn klaw_does_not_fire_on_land_played_from_hand() {
    let mut scenario = GameScenario::new_n_player(2, 17);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Klaw, Master of Sound", 2, 3, KLAW_ORACLE);

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let land = create_object(
        runner.state_mut(),
        CardId(50_001),
        P0,
        "Some Land".to_string(),
        Zone::Battlefield,
    );

    let life_before = runner.life(P0);
    let events = vec![GameEvent::LandPlayed {
        object_id: land,
        player_id: P0,
        from_zone: Zone::Hand,
    }];
    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    assert_eq!(
        runner.life(P0),
        life_before,
        "playing a land from hand must NOT fire the from-exile trigger"
    );
}

/// Negative: an OPPONENT playing a land from exile must not fire Klaw
/// (`valid_target = Controller`).
#[test]
fn klaw_does_not_fire_on_opponent_land_from_exile() {
    let mut scenario = GameScenario::new_n_player(2, 19);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Klaw, Master of Sound", 2, 3, KLAW_ORACLE);

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    let land = create_object(
        runner.state_mut(),
        CardId(50_002),
        P1,
        "Opp Land".to_string(),
        Zone::Battlefield,
    );

    let life_before = runner.life(P0);
    let events = vec![GameEvent::LandPlayed {
        object_id: land,
        player_id: P1,
        from_zone: Zone::Exile,
    }];
    process_triggers(runner.state_mut(), &events);
    drain_priority(&mut runner);

    assert_eq!(
        runner.life(P0),
        life_before,
        "an opponent playing a land from exile must not fire Klaw (controller-scoped)"
    );
}

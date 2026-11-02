//! Production action coverage for starting-life references on printed cards.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::format::FormatConfig;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ELENDA: &str = "Lifelink, hexproof from instants\nAs long as your life total is greater than your starting life total, Elenda gets +1/+1 and has menace. Elenda gets an additional +5/+5 as long as your life total is at least 10 greater than your starting life total.";
const TORGAAR: &str = "As an additional cost to cast this spell, you may sacrifice any number of creatures. This spell costs {2} less to cast for each creature sacrificed this way.\nWhen Torgaar enters, up to one target player's life total becomes half their starting life total, rounded down.";

fn assert_elenda(runner: &GameRunner, id: ObjectId, pt: i32, menace: bool) {
    let object = &runner.state().objects[&id];
    assert_eq!((object.power, object.toughness), (Some(pt), Some(pt)));
    assert_eq!(object.has_keyword(&Keyword::Menace), menace);
    assert!(object.has_keyword(&Keyword::Lifelink));
}

#[test]
fn elenda_full_oracle_reacts_to_life_actions_without_forced_layer_pass() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let elenda = scenario
        .add_creature(P0, "Elenda, Saint of Dusk", 4, 4)
        .from_oracle_text_with_keywords(&["Lifelink", "Hexproof", "Hexproof from"], ELENDA)
        .id();
    let gain_one = scenario
        .add_spell_to_hand_from_oracle(P0, "Gain One", false, "You gain 1 life.")
        .id();
    let gain_nine = scenario
        .add_spell_to_hand_from_oracle(P0, "Gain Nine", false, "You gain 9 life.")
        .id();
    let lose_one = scenario
        .add_spell_to_hand_from_oracle(P0, "Lose One", false, "You lose 1 life.")
        .id();
    let lose_nine = scenario
        .add_spell_to_hand_from_oracle(P0, "Lose Nine", false, "You lose 9 life.")
        .id();
    let mut runner = scenario.build();
    assert_eq!(runner.life(P0), 20);

    let outcome = runner.cast(gain_one).resolve();
    outcome.assert_life_delta(P0, 1);
    assert_elenda(&runner, elenda, 5, true);

    let outcome = runner.cast(gain_nine).resolve();
    outcome.assert_life_delta(P0, 9);
    assert_elenda(&runner, elenda, 10, true);

    let outcome = runner.cast(lose_one).resolve();
    outcome.assert_life_delta(P0, -1);
    assert_elenda(&runner, elenda, 5, true);

    let outcome = runner.cast(lose_nine).resolve();
    outcome.assert_life_delta(P0, -9);
    assert_elenda(&runner, elenda, 4, false);
}

#[test]
fn torgaar_etb_targets_archenemy_and_halves_targets_starting_life() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P1);
    let mut scenario = GameScenario::new_with_format(format, 3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P1, 30);
    let torgaar = scenario
        .add_creature_to_hand_from_oracle(P0, "Torgaar, Famine Incarnate", 7, 6, TORGAAR)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = engine::types::game_state::WaitingFor::Priority { player: P0 };
    assert_eq!(runner.life(P0), 20);
    assert_eq!(runner.life(P1), 30);

    let outcome = runner.cast(torgaar).target_player(P1).resolve();
    assert_eq!(outcome.zone_of(torgaar), Zone::Battlefield);
    outcome.assert_life_delta(P1, -10);
    outcome.assert_life_delta(P0, 0);
    assert_eq!(runner.life(P1), 20);
}

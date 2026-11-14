//! Nahiri, Storm of Stone: both conjuncts of the static, each under "During your turn".
//!
//! Oracle (WAR): "During your turn, creatures you control have first strike and equip
//! abilities you activate cost {1} less to activate.
//! −X: Nahiri deals X damage to target tapped creature."
//!
//! CR 604.1 + CR 611.3a (static abilities), CR 601.2f + CR 602.2b (activation cost
//! reductions), CR 702.6a (equip is an activated ability of Equipment).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const NAHIRI: &str = "During your turn, creatures you control have first strike and equip abilities you activate cost {1} less to activate.\n[−X]: Nahiri deals X damage to target tapped creature.";
const LEONIN_SHIKARI: &str = "You may activate equip abilities any time you could cast an instant.";

fn p0_pool(runner: &GameRunner) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool
        .total()
}

struct Board {
    runner: GameRunner,
    nahiri: ObjectId,
    bear: ObjectId,
    sword: ObjectId,
}

/// Main phase: Nahiri under `nahiri_controller`, P0's vanilla creature, P0's Equip {3}
/// Equipment, and six mana in P0's pool. `shikari` adds P0's Leonin Shikari so P0 may
/// equip at instant speed.
fn board(nahiri_controller: PlayerId, shikari: bool) -> Board {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let nahiri = s
        .add_planeswalker_from_oracle(
            nahiri_controller,
            "Nahiri, Storm of Stone",
            "Nahiri",
            6,
            NAHIRI,
        )
        .id();
    let bear = s.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let sword = s
        .add_artifact_from_oracle(P0, "Practice Sword", "Equip {3}")
        .with_subtypes(vec!["Equipment"])
        .id();
    if shikari {
        s.add_creature_from_oracle(P0, "Leonin Shikari", 2, 2, LEONIN_SHIKARI);
    }
    s.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaColor::White.into(), ObjectId(0), false, Vec::new()))
            .collect(),
    );
    let mut runner = s.build();
    engine::game::layers::evaluate_layers(runner.state_mut());
    Board {
        runner,
        nahiri,
        bear,
        sword,
    }
}

/// Equip `sword` onto `bear` as P0 and return P0's mana left (six before).
fn equip_and_pool(b: &mut Board) -> usize {
    b.runner
        .activate(b.sword, 0)
        .target_object(b.bear)
        .resolve();
    assert!(
        b.runner.state().objects[&b.bear]
            .attachments
            .contains(&b.sword),
        "reach: the equip ability resolved and attached the Equipment"
    );
    p0_pool(&b.runner)
}

#[test]
fn nahiri_storm_of_stone_discounts_equip_during_your_turn() {
    let mut b = board(P0, false);
    assert!(
        b.runner.state().objects[&b.bear].has_keyword(&Keyword::FirstStrike),
        "reach: Nahiri's first conjunct (first strike during your turn) must apply"
    );
    assert_eq!(
        equip_and_pool(&mut b),
        4,
        "Equip {{3}} must cost {{2}} during P0's turn with Nahiri out (6 mana -> 4 left)"
    );
}

#[test]
fn nahiri_storm_of_stone_does_not_grant_equip_keyword_to_creatures() {
    let b = board(P0, false);
    let obj = &b.runner.state().objects[&b.bear];
    assert!(
        obj.has_keyword(&Keyword::FirstStrike),
        "reach: Nahiri's first conjunct (first strike during your turn) must apply"
    );
    assert!(
        !obj.keywords.iter().any(|k| matches!(k, Keyword::Equip(_))),
        "the cost-reduction conjunct must not grant creatures an Equip keyword; keywords = {:?}",
        obj.keywords
    );
}

/// CR 604.1: "During your turn" scopes the discount too. Leonin Shikari lets P0 equip
/// during P1's turn, where Nahiri's discount must not apply.
#[test]
fn nahiri_storm_of_stone_discount_is_off_during_opponents_turn() {
    let mut b = board(P0, true);
    assert!(
        b.runner.state().objects[&b.nahiri]
            .static_definitions
            .as_slice()
            .iter()
            .any(|def| matches!(def.mode, StaticMode::ReduceAbilityCost { .. })),
        "reach: Nahiri carries the equip discount static"
    );
    {
        let state = b.runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    engine::game::layers::evaluate_layers(b.runner.state_mut());
    assert!(
        !b.runner.state().objects[&b.bear].has_keyword(&Keyword::FirstStrike),
        "first strike is also off during the opponent's turn"
    );
    assert_eq!(
        equip_and_pool(&mut b),
        3,
        "Equip {{3}} must cost the full {{3}} during the opponent's turn (6 mana -> 3 left)"
    );
}

/// An opponent's Nahiri discounts only that opponent's equip abilities ("you activate").
#[test]
fn opponents_nahiri_storm_of_stone_does_not_discount_your_equip() {
    let mut b = board(P1, false);
    assert!(
        !b.runner.state().objects[&b.bear].has_keyword(&Keyword::FirstStrike),
        "an opponent's Nahiri does not grant P0's creatures first strike"
    );
    assert_eq!(
        equip_and_pool(&mut b),
        3,
        "P0's Equip {{3}} costs the full {{3}} under an opponent's Nahiri"
    );
}

/// CR 611.3a: the discount ends when Nahiri leaves the battlefield.
#[test]
fn nahiri_storm_of_stone_discount_ends_when_she_leaves() {
    let mut b = board(P0, false);
    assert!(
        b.runner.state().objects[&b.bear].has_keyword(&Keyword::FirstStrike),
        "reach: Nahiri's static applied before she leaves"
    );
    engine::game::zones::move_to_zone(
        b.runner.state_mut(),
        b.nahiri,
        Zone::Graveyard,
        &mut Vec::new(),
    );
    engine::game::layers::evaluate_layers(b.runner.state_mut());
    assert!(
        !b.runner.state().objects[&b.bear].has_keyword(&Keyword::FirstStrike),
        "first strike ends with Nahiri gone"
    );
    assert_eq!(
        equip_and_pool(&mut b),
        3,
        "Equip {{3}} costs the full {{3}} once Nahiri has left the battlefield"
    );
}

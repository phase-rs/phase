//! Nahiri, Storm of Stone: both conjuncts of the static, each under "During your turn".
//!
//! Oracle (WAR): "During your turn, creatures you control have first strike and equip
//! abilities you activate cost {1} less to activate.
//! −X: Nahiri deals X damage to target tapped creature."
//!
//! CR 604.1 + CR 611.3a (static abilities), CR 601.2f + CR 602.2b (activation cost
//! reductions), CR 118.7a (a generic reduction affects only the generic component),
//! CR 702.6a (equip is an activated ability of Equipment).

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const NAHIRI: &str = "During your turn, creatures you control have first strike and equip abilities you activate cost {1} less to activate.\n[−X]: Nahiri deals X damage to target tapped creature.";
const GREATSWORD: &str = "Equipped creature gets +3/+0.\nEquip {3}";
const LEONIN_SHIKARI: &str = "You may activate equip abilities any time you could cast an instant.";
const HEROS_DOWNFALL: &str = "Destroy target creature or planeswalker.";

fn pool(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool
        .total()
}

fn mana(color: ManaColor, n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(color.into(), ObjectId(0), false, Vec::new()))
        .collect()
}

/// One player's side: a vanilla creature, a Greatsword and six mana.
struct Side {
    bear: ObjectId,
    sword: ObjectId,
}

fn add_side(s: &mut GameScenario, player: PlayerId, shikari: bool) -> Side {
    let bear = s.add_creature(player, "Grizzly Bears", 2, 2).id();
    let sword = s
        .add_artifact_from_oracle(player, "Greatsword", GREATSWORD)
        .with_subtypes(vec!["Equipment"])
        .id();
    if shikari {
        s.add_creature_from_oracle(player, "Leonin Shikari", 2, 2, LEONIN_SHIKARI);
    }
    s.with_mana_pool(player, mana(ManaColor::White, 6));
    Side { bear, sword }
}

fn give_priority(runner: &mut GameRunner, active: PlayerId, priority: PlayerId) {
    let state = runner.state_mut();
    state.active_player = active;
    state.priority_player = priority;
    state.waiting_for = WaitingFor::Priority { player: priority };
}

fn pass_priority(runner: &mut GameRunner) {
    runner
        .act(GameAction::PassPriority)
        .expect("passing priority must be accepted");
}

/// CR 500.1 + CR 117.3d: pass priority (declaring no attackers) until P1's turn
/// begins and P0 receives priority in it.
fn pass_into_p1_turn_until_p0_has_priority(runner: &mut GameRunner) {
    for _ in 0..64 {
        let state = runner.state();
        match state.waiting_for {
            WaitingFor::Priority { player } if state.active_player == P1 && player == P0 => {
                return;
            }
            WaitingFor::Priority { .. } => pass_priority(runner),
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .declare_attackers(&[])
                    .expect("declaring no attackers must be accepted");
            }
            ref other => panic!("unexpected prompt while passing the turn: {other:?}"),
        }
    }
    panic!("P0 never received priority during P1's turn");
}

/// CR 605.3a: tap `land` for mana through the offered mana-ability action.
fn tap_for_mana(runner: &mut GameRunner, land: ObjectId) {
    let (_, _, grouped) = legal_actions_full(runner.state());
    let action = grouped
        .get(&land)
        .and_then(|actions| {
            actions.iter().find(|action| {
                matches!(action, GameAction::TapLandForMana { selection }
                    if selection.source.object_id == land)
            })
        })
        .cloned()
        .expect("tapping the land for mana must be offered");
    runner
        .act(action)
        .expect("tapping the land for mana must be accepted");
}

/// Equip `side.sword` onto `target` as the current priority player; return the mana
/// that player spent.
fn equip_cost(runner: &mut GameRunner, player: PlayerId, side: &Side, target: ObjectId) -> usize {
    let before = pool(runner, player);
    runner
        .activate(side.sword, 0)
        .target_object(target)
        .resolve();
    assert!(
        runner.state().objects[&target]
            .attachments
            .contains(&side.sword),
        "reach: the equip ability resolved and attached the Greatsword"
    );
    before - pool(runner, player)
}

/// P0's main phase with P0's Nahiri and P0's side.
fn p0_board(shikari: bool) -> (GameRunner, ObjectId, Side) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let nahiri = s
        .add_planeswalker_from_oracle(P0, "Nahiri, Storm of Stone", "Nahiri", 6, NAHIRI)
        .id();
    let side = add_side(&mut s, P0, shikari);
    let mut runner = s.build();
    engine::game::layers::evaluate_layers(runner.state_mut());
    (runner, nahiri, side)
}

#[test]
fn nahiri_storm_of_stone_discounts_equip_during_your_turn() {
    let (mut runner, _, side) = p0_board(false);
    assert!(
        runner.state().objects[&side.bear].has_keyword(&Keyword::FirstStrike),
        "reach: Nahiri's first conjunct (first strike during your turn) must apply"
    );
    assert_eq!(
        equip_cost(&mut runner, P0, &side, side.bear),
        2,
        "Equip {{3}} must cost {{2}} during P0's turn with Nahiri out"
    );
}

#[test]
fn nahiri_storm_of_stone_does_not_grant_equip_keyword_to_creatures() {
    let (runner, _, side) = p0_board(false);
    let obj = &runner.state().objects[&side.bear];
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

/// CR 604.1 + CR 109.5: "During your turn" scopes both conjuncts to Nahiri's
/// controller's turn. On one board, P0's Nahiri gives first strike and a {2} equip on
/// P0's turn. The players then pass priority into P1's turn (CR 500.1); P0's floating
/// mana empties (CR 500.5), P0 receives priority in P1's turn, taps three Plains,
/// and equips through Leonin Shikari (CR 117.1a) with both effects gone.
#[test]
fn nahiri_storm_of_stone_effects_end_when_the_turn_changes() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_planeswalker_from_oracle(P0, "Nahiri, Storm of Stone", "Nahiri", 6, NAHIRI);
    let side = add_side(&mut s, P0, true);
    let second_bear = s.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let plains: Vec<ObjectId> = (0..3)
        .map(|_| s.add_basic_land(P0, ManaColor::White))
        .collect();
    let mut runner = s.build();
    engine::game::layers::evaluate_layers(runner.state_mut());

    assert!(
        runner.state().objects[&side.bear].has_keyword(&Keyword::FirstStrike),
        "reach: on P0's turn, Nahiri gives P0's creatures first strike"
    );
    assert_eq!(
        equip_cost(&mut runner, P0, &side, side.bear),
        2,
        "reach: on P0's turn, Nahiri discounts P0's Equip {{3}} to {{2}}"
    );

    let turn_before = runner.state().turn_number;
    pass_into_p1_turn_until_p0_has_priority(&mut runner);
    assert_eq!(
        (runner.state().turn_number, runner.state().active_player),
        (turn_before + 1, P1),
        "reach: the game advanced into P1's next turn"
    );
    assert_eq!(
        pool(&runner, P0),
        0,
        "reach: P0's floating mana emptied during the turn change"
    );
    assert!(
        !runner.state().objects[&side.bear].has_keyword(&Keyword::FirstStrike),
        "first strike ends when the turn passes to P1"
    );
    for land in &plains {
        tap_for_mana(&mut runner, *land);
    }
    assert_eq!(pool(&runner, P0), 3, "reach: three Plains tapped for mana");
    assert_eq!(
        equip_cost(&mut runner, P0, &side, second_bear),
        3,
        "Equip {{3}} costs the full {{3}} once the turn passes to P1"
    );
}

/// CR 602.2: "equip abilities YOU activate" — on her controller's turn, where the
/// static is active, P1's Nahiri discounts P1's equip but not P0's (P0 equips at
/// instant speed through Leonin Shikari).
#[test]
fn nahiri_storm_of_stone_discounts_only_her_controllers_equip() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_planeswalker_from_oracle(P1, "Nahiri, Storm of Stone", "Nahiri", 6, NAHIRI);
    let mine = add_side(&mut s, P0, true);
    let hers = add_side(&mut s, P1, false);
    let mut runner = s.build();
    give_priority(&mut runner, P1, P1);
    engine::game::layers::evaluate_layers(runner.state_mut());

    assert_eq!(
        equip_cost(&mut runner, P1, &hers, hers.bear),
        2,
        "reach: on P1's turn, P1's Nahiri discounts P1's own Equip {{3}} to {{2}}"
    );
    pass_priority(&mut runner);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "reach: P1 passed priority to P0"
    );
    assert_eq!(
        equip_cost(&mut runner, P0, &mine, mine.bear),
        3,
        "P1's Nahiri must not discount the equip ability P0 activates"
    );
}

/// CR 611.3b: the discount applies only while Nahiri is on the battlefield. A
/// discounted equip, then an opponent's Hero's Downfall destroys Nahiri, then the
/// same Greatsword's equip costs the full {3}.
#[test]
fn nahiri_storm_of_stone_discount_ends_when_she_is_destroyed() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let nahiri = s
        .add_planeswalker_from_oracle(P0, "Nahiri, Storm of Stone", "Nahiri", 6, NAHIRI)
        .id();
    let side = add_side(&mut s, P0, false);
    let second_bear = s.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let downfall = s
        .add_spell_to_hand_from_oracle(P1, "Hero's Downfall", true, HEROS_DOWNFALL)
        .id();
    s.with_mana_pool(P1, mana(ManaColor::Black, 3));
    let mut runner = s.build();
    engine::game::layers::evaluate_layers(runner.state_mut());

    assert_eq!(
        equip_cost(&mut runner, P0, &side, side.bear),
        2,
        "reach: with Nahiri out, Equip {{3}} costs {{2}}"
    );

    pass_priority(&mut runner);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 },
        "reach: P0 passed priority to P1"
    );
    runner.cast(downfall).target_object(nahiri).resolve();
    assert_eq!(
        runner.state().objects[&nahiri].zone,
        Zone::Graveyard,
        "reach: Hero's Downfall destroyed Nahiri"
    );
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "reach: CR 117.3b — the active player P0 receives priority after it resolves"
    );
    assert!(
        !runner.state().objects[&side.bear].has_keyword(&Keyword::FirstStrike),
        "first strike ends with Nahiri gone"
    );
    assert_eq!(
        equip_cost(&mut runner, P0, &side, second_bear),
        3,
        "Equip {{3}} costs the full {{3}} once Nahiri has left the battlefield"
    );
}

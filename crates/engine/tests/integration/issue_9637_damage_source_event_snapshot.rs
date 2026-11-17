//! Issue #9637 — a damage trigger's source qualifier is evaluated against the
//! source as it was when it dealt the damage (CR 603.2 + CR 120.1).
//!
//! A spell deals its damage while on the stack, then leaves the stack before
//! the engine collects the triggers that damage caused. Once it has moved it is
//! a new object (CR 400.7), and an Adventure card is also back to its creature
//! characteristics (CR 715.3b + CR 715.4). Frolicking Familiar // Blow Off
//! Steam is a red instant on the stack and a blue creature card in exile, so it
//! separates a qualifier read from the damage event from one read from the
//! card afterwards.
//!
//! Every damage-trigger matcher shares that read, so each path has a printed
//! card here:
//! - `DamageReceived` (passive voice): Chandra's Phoenix, "Whenever an opponent
//!   is dealt damage by a red instant or sorcery spell you control or by a red
//!   planeswalker you control, return this card from your graveyard to your
//!   hand."
//! - `DamageDone` (active voice): Tamanoa, "Whenever a noncreature source you
//!   control deals damage, you gain that much life."
//!
//! The batched `DamageDoneOnceByController` path and the aggregate
//! combat-damage path share the same helper and are covered at the matcher
//! level in `game/trigger_matchers.rs`.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn add_mana(runner: &mut GameRunner, player: PlayerId, color: ManaType, count: usize) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(color, ObjectId(0), false, Vec::new()));
    }
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .life
}

/// P0's main phase with `permanents` on P0's battlefield, Chandra's Phoenix in
/// P0's graveyard, and Frolicking Familiar in P0's hand with {R} to cast Blow
/// Off Steam ({R}).
fn setup(permanents: &[&str]) -> Option<(GameRunner, ObjectId, ObjectId, Vec<ObjectId>)> {
    let db = load_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let phoenix = scenario.add_real_card(P0, "Chandra's Phoenix", Zone::Graveyard, db);
    let familiar = scenario.add_real_card(P0, "Frolicking Familiar", Zone::Hand, db);
    let permanents = permanents
        .iter()
        .map(|name| scenario.add_real_card(P0, name, Zone::Battlefield, db))
        .collect();
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana(&mut runner, P0, ManaType::Red, 1);
    Some((runner, phoenix, familiar, permanents))
}

/// Casts Frolicking Familiar as Blow Off Steam (CR 715.3a) targeting P1 and
/// resolves it and every trigger it causes.
fn blow_off_steam_at_opponent(runner: &mut GameRunner, familiar: ObjectId) {
    let card_id = runner.state().objects[&familiar].card_id;
    let offer = runner
        .act(GameAction::CastSpell {
            object_id: familiar,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Frolicking Familiar")
        .waiting_for;
    assert!(
        matches!(
            offer,
            WaitingFor::CastOffer {
                player: P0,
                kind: CastOfferKind::Adventure { .. },
            }
        ),
        "CR 715.3a: the caster chooses whether to cast the Adventure, got {offer:?}"
    );
    let targeting = runner
        .act(GameAction::ChooseAdventureFace { creature: false })
        .expect("choose Blow Off Steam")
        .waiting_for;
    assert!(
        matches!(targeting, WaitingFor::TargetSelection { .. }),
        "Blow Off Steam targets any target, got {targeting:?}"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("target the opponent");
    runner.advance_until_stack_empty();
}

/// Attacks P1 with `attacker`, declares no blockers, and deals combat damage.
fn attack_opponent_unblocked(runner: &mut GameRunner, attacker: ObjectId) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    for _ in 0..8 {
        if runner.waiting_for_kind() == "DeclareBlockers" {
            runner.declare_blockers(&[]).expect("declare no blockers");
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority into the declare-blockers step");
    }
    runner.combat_damage();
    runner.advance_until_stack_empty();
}

fn assert_settled(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty(), "the stack must be empty");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected a priority window, got {:?}",
        runner.state().waiting_for
    );
}

/// The reviewed witness: Blow Off Steam is a red instant spell P0 controls when
/// it deals the damage, so Chandra's Phoenix returns even though the card is a
/// blue creature card in exile by the time the trigger is collected.
#[test]
fn chandras_phoenix_returns_from_adventure_instant_damage() {
    let Some((mut runner, phoenix, familiar, _)) = setup(&[]) else {
        return;
    };
    let p1_life = life(&runner, P1);

    blow_off_steam_at_opponent(&mut runner, familiar);

    assert_eq!(life(&runner, P1), p1_life - 1, "Blow Off Steam dealt 1");
    assert_eq!(
        runner.state().objects[&familiar].zone,
        Zone::Exile,
        "CR 715.3d: the Adventure resolved into exile as its creature card"
    );
    assert_eq!(
        runner.state().objects[&phoenix].zone,
        Zone::Hand,
        "CR 603.2: damage from a red instant spell P0 controls returns Chandra's Phoenix"
    );
    assert_settled(&runner);
}

/// The planeswalker leg of the source disjunction: a red planeswalker P0
/// controls (Chandra Nalaar, still on the battlefield) also returns Phoenix.
#[test]
fn chandras_phoenix_returns_from_red_planeswalker_damage() {
    let Some((mut runner, phoenix, _, permanents)) = setup(&["Chandra Nalaar"]) else {
        return;
    };
    let chandra = permanents[0];
    let p1_life = life(&runner, P1);

    runner.activate(chandra, 0).target_player(P1).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        life(&runner, P1),
        p1_life - 1,
        "Chandra Nalaar's +1 dealt 1"
    );
    assert_eq!(
        runner.state().objects[&phoenix].zone,
        Zone::Hand,
        "damage from a red planeswalker P0 controls returns Chandra's Phoenix"
    );
    assert_settled(&runner);
}

/// The negative control: combat damage from a red creature is neither from an
/// instant or sorcery spell nor from a planeswalker, so Phoenix stays put.
#[test]
fn chandras_phoenix_ignores_red_creature_damage() {
    let Some((mut runner, phoenix, _, permanents)) = setup(&["Raging Goblin"]) else {
        return;
    };
    let goblin = permanents[0];
    let p1_life = life(&runner, P1);

    attack_opponent_unblocked(&mut runner, goblin);

    assert_eq!(
        life(&runner, P1),
        p1_life - 1,
        "reach guard: the red creature dealt combat damage to the opponent"
    );
    assert_eq!(
        runner.state().objects[&phoenix].zone,
        Zone::Graveyard,
        "a red creature does not satisfy Chandra's Phoenix's source qualifier"
    );
}

/// The `DamageDone` path: Blow Off Steam is a noncreature source P0 controls
/// when it deals the damage, so Tamanoa gains P0 that much life even though
/// the card is a creature card by the time the trigger is collected.
#[test]
fn tamanoa_gains_life_from_adventure_instant_damage() {
    let Some((mut runner, _, familiar, _)) = setup(&["Tamanoa"]) else {
        return;
    };
    let p0_life = life(&runner, P0);
    let p1_life = life(&runner, P1);

    blow_off_steam_at_opponent(&mut runner, familiar);

    assert_eq!(life(&runner, P1), p1_life - 1, "Blow Off Steam dealt 1");
    assert_eq!(
        life(&runner, P0),
        p0_life + 1,
        "CR 603.2: a noncreature source P0 controls dealt 1, so Tamanoa gains 1"
    );
    assert_settled(&runner);
}

/// The `DamageDone` negative control: a creature's combat damage is not from a
/// noncreature source, so Tamanoa gains nothing.
#[test]
fn tamanoa_ignores_creature_damage() {
    let Some((mut runner, _, _, permanents)) = setup(&["Tamanoa", "Raging Goblin"]) else {
        return;
    };
    let goblin = permanents[1];
    let p0_life = life(&runner, P0);
    let p1_life = life(&runner, P1);

    attack_opponent_unblocked(&mut runner, goblin);

    assert_eq!(
        life(&runner, P1),
        p1_life - 1,
        "reach guard: the creature dealt combat damage to the opponent"
    );
    assert_eq!(
        life(&runner, P0),
        p0_life,
        "a creature source does not satisfy Tamanoa's noncreature qualifier"
    );
}

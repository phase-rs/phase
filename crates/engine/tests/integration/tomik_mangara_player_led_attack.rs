//! Tomik, Wielder of Law and Mangara, the Diplomat: "Whenever an opponent
//! attacks with creatures, if two or more of those creatures are attacking you
//! and/or planeswalkers you control, ..." fires once for the attacking
//! opponent's declaration (CR 508.3c), counts that opponent's whole attack, and
//! Tomik's "that opponent" is the attacking player.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::actions::GameAction;
use engine::types::game_state::{PersistedGameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::Zone;

const TOMIK: &str = "Affinity for planeswalkers (This spell costs {1} less to cast for each planeswalker you control.)\nFlying, vigilance\nWhenever an opponent attacks with creatures, if two or more of those creatures are attacking you and/or planeswalkers you control, that opponent loses 3 life and you draw a card.";
const MANGARA: &str = "Lifelink\nWhenever an opponent attacks with creatures, if two or more of those creatures are attacking you and/or planeswalkers you control, draw a card.\nWhenever an opponent casts their second spell each turn, draw a card.";
const JACE_BELEREN: &str = "+2: Each player draws a card.\n\u{2212}1: Target player draws a card.\n\u{2212}10: Target player mills twenty cards.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";
const ACT_OF_AGGRESSION: &str = "({R/P} can be paid with either {R} or 2 life.)\nGain control of target creature an opponent controls until end of turn. Untap that creature. It gains haste until end of turn.";

struct Board {
    runner: GameRunner,
    observer: ObjectId,
    planeswalker: ObjectId,
    attackers: Vec<ObjectId>,
    draw: ObjectId,
    steal: ObjectId,
    bounce: ObjectId,
}

/// P0 attacks; `defender` controls the observer and a planeswalker.
fn board(players: u8, defender: PlayerId, name: &str, oracle: &str, attackers: usize) -> Board {
    let mut scenario = GameScenario::new_n_player(players, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let observer = scenario
        .add_creature_from_oracle(defender, name, 2, 4, oracle)
        .id();
    let planeswalker = scenario
        .add_planeswalker_from_oracle(defender, "Jace Beleren", "Jace", 3, JACE_BELEREN)
        .id();
    let draw = scenario.add_card_to_library_top(defender, "Island");
    let steal = scenario
        .add_spell_to_hand_from_oracle(defender, "Act of Aggression", true, ACT_OF_AGGRESSION)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(defender, "Unsummon", true, UNSUMMON)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let attackers = (0..attackers)
        .map(|i| {
            scenario
                .add_creature(P0, &format!("Grizzly Bears {i}"), 2, 2)
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    Board {
        runner,
        observer,
        planeswalker,
        attackers,
        draw,
        steal,
        bounce,
    }
}

fn observer_triggers(runner: &GameRunner, observer: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == observer)
        .count()
}

/// CR 508.3c + CR 603.4: one attacker at you and one at your planeswalker
/// meet the threshold; the attacking opponent loses 3 and you draw once.
#[test]
fn tomik_drains_the_attacking_opponent_for_an_attack_on_you_and_your_planeswalker() {
    let mut b = board(2, P1, "Tomik, Wielder of Law", TOMIK, 2);
    let (p0_life, p1_life) = (b.runner.life(P0), b.runner.life(P1));
    b.runner
        .declare_attackers(&[
            (b.attackers[0], AttackTarget::Player(P1)),
            (b.attackers[1], AttackTarget::Planeswalker(b.planeswalker)),
        ])
        .expect("attack you and your planeswalker");
    assert_eq!(observer_triggers(&b.runner, b.observer), 1);
    b.runner.advance_until_stack_empty();
    assert_eq!(b.runner.life(P0), p0_life - 3);
    assert_eq!(b.runner.life(P1), p1_life);
    assert_eq!(b.runner.state().objects[&b.draw].zone, Zone::Hand);
}

/// CR 603.4: one qualifying attacker is below the threshold; three still
/// make one trigger and one draw.
#[test]
fn tomik_threshold_counts_the_whole_declaration_once() {
    let mut one = board(2, P1, "Tomik, Wielder of Law", TOMIK, 1);
    let p0_life = one.runner.life(P0);
    one.runner
        .declare_attackers(&[(one.attackers[0], AttackTarget::Player(P1))])
        .expect("attack with one creature");
    assert_eq!(observer_triggers(&one.runner, one.observer), 0);
    one.runner.advance_until_stack_empty();
    assert_eq!(one.runner.life(P0), p0_life);

    let mut three = board(2, P1, "Tomik, Wielder of Law", TOMIK, 3);
    let p0_life = three.runner.life(P0);
    let hand_before = three.runner.state().players[usize::from(P1.0)].hand.len();
    let attacks: Vec<_> = three
        .attackers
        .iter()
        .map(|&id| (id, AttackTarget::Player(P1)))
        .collect();
    three
        .runner
        .declare_attackers(&attacks)
        .expect("attack with three creatures");
    assert_eq!(observer_triggers(&three.runner, three.observer), 1);
    three.runner.advance_until_stack_empty();
    assert_eq!(three.runner.life(P0), p0_life - 3);
    assert_eq!(
        three.runner.state().players[usize::from(P1.0)].hand.len(),
        hand_before + 1
    );
}

/// CR 508.3c + CR 508.5: in a three-player game the attacker splitting its
/// attack still makes one trigger, and only attackers aimed at Tomik's
/// controller or their planeswalkers count.
#[test]
fn tomik_in_multiplayer_counts_only_attackers_aimed_at_its_controller() {
    let p2 = PlayerId(2);
    let mut split = board(3, P1, "Tomik, Wielder of Law", TOMIK, 3);
    let lives = [
        split.runner.life(P0),
        split.runner.life(P1),
        split.runner.life(p2),
    ];
    split
        .runner
        .declare_attackers(&[
            (split.attackers[0], AttackTarget::Player(P1)),
            (split.attackers[1], AttackTarget::Player(P1)),
            (split.attackers[2], AttackTarget::Player(p2)),
        ])
        .expect("split the attack between both opponents");
    assert_eq!(observer_triggers(&split.runner, split.observer), 1);
    split.runner.advance_until_stack_empty();
    assert_eq!(
        [
            split.runner.life(P0),
            split.runner.life(P1),
            split.runner.life(p2)
        ],
        [lives[0] - 3, lives[1], lives[2]]
    );

    let mut elsewhere = board(3, P1, "Tomik, Wielder of Law", TOMIK, 2);
    let p0_life = elsewhere.runner.life(P0);
    let attacks: Vec<_> = elsewhere
        .attackers
        .iter()
        .map(|&id| (id, AttackTarget::Player(p2)))
        .collect();
    elsewhere
        .runner
        .declare_attackers(&attacks)
        .expect("attack the other opponent");
    assert_eq!(observer_triggers(&elsewhere.runner, elsewhere.observer), 0);
    elsewhere.runner.advance_until_stack_empty();
    assert_eq!(elsewhere.runner.life(P0), p0_life);
}

/// CR 508.3c: Mangara shares Tomik's head and draws exactly once.
#[test]
fn mangara_draws_once_for_an_attack_on_you_and_your_planeswalker() {
    let mut b = board(2, P1, "Mangara, the Diplomat", MANGARA, 3);
    let hand_before = b.runner.state().players[usize::from(P1.0)].hand.len();
    b.runner
        .declare_attackers(&[
            (b.attackers[0], AttackTarget::Player(P1)),
            (b.attackers[1], AttackTarget::Planeswalker(b.planeswalker)),
            (b.attackers[2], AttackTarget::Player(P1)),
        ])
        .expect("attack you and your planeswalker");
    assert_eq!(observer_triggers(&b.runner, b.observer), 1);
    b.runner.advance_until_stack_empty();
    assert_eq!(
        b.runner.state().players[usize::from(P1.0)].hand.len(),
        hand_before + 1
    );
    assert_eq!(b.runner.state().objects[&b.draw].zone, Zone::Hand);
}

/// CR 603.4 + CR 506.4: Stealing an attacker removes it from combat before
/// Tomik rechecks the threshold. If the trigger resolves, "that opponent"
/// still names the player who declared the attack (CR 508.3c).
#[test]
fn tomik_rechecks_the_threshold_and_keeps_the_declaring_player_after_a_control_change() {
    for (initial_count, expected_life_loss, expected_draw_zone) in
        [(2, 0, Zone::Library), (3, 3, Zone::Hand)]
    {
        let mut b = board(2, P1, "Tomik, Wielder of Law", TOMIK, initial_count);
        let (p0_life, p1_life) = (b.runner.life(P0), b.runner.life(P1));
        let attacks: Vec<_> = b
            .attackers
            .iter()
            .map(|&id| (id, AttackTarget::Player(P1)))
            .collect();
        b.runner
            .declare_attackers(&attacks)
            .expect("declare qualifying attackers at Tomik's controller");
        assert_eq!(observer_triggers(&b.runner, b.observer), 1);

        // Taking the first declared attacker removes it from combat. Its new
        // controller differs from the opponent who declared the attack.
        let first = b.attackers[0];
        steal_attacker(&mut b, first);
        assert_eq!(observer_triggers(&b.runner, b.observer), 1);

        b.runner.advance_until_stack_empty();
        assert_eq!(
            b.runner.life(P0),
            p0_life - expected_life_loss,
            "{initial_count} declared"
        );
        assert_eq!(b.runner.life(P1), p1_life);
        assert_eq!(
            b.runner.state().objects[&b.draw].zone,
            expected_draw_zone,
            "{initial_count} declared"
        );
        assert!(b.runner.state().stack.is_empty());
        assert!(matches!(
            b.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

fn steal_attacker(b: &mut Board, attacker: ObjectId) {
    b.runner
        .act(GameAction::PassPriority)
        .expect("the attacking player passes");
    assert!(matches!(
        b.runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    b.runner.cast(b.steal).target_object(attacker).commit();
    b.runner.resolve_top();
    assert_eq!(b.runner.state().objects[&attacker].controller, P1);

    assert!(!b
        .runner
        .state()
        .combat
        .as_ref()
        .unwrap()
        .attackers
        .iter()
        .any(|info| info.object_id == attacker));
}

/// P1 returns `attacker` to its owner's hand with Unsummon while Mangara's
/// trigger waits.
fn bounce_attacker(b: &mut Board, attacker: ObjectId) {
    if matches!(
        b.runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ) {
        b.runner
            .act(GameAction::PassPriority)
            .expect("the attacking player passes");
    }
    b.runner.cast(b.bounce).target_object(attacker).commit();
    b.runner.resolve_top();
    assert_eq!(b.runner.state().objects[&attacker].zone, Zone::Hand);
}

/// Mangara ruling + CR 608.2h: an attacker that left the battlefield still
/// counts where it was attacking when it left.
#[test]
fn mangara_counts_an_attacker_that_left_the_battlefield() {
    let mut b = board(2, P1, "Mangara, the Diplomat", MANGARA, 2);
    let attacks: Vec<_> = b
        .attackers
        .iter()
        .map(|&id| (id, AttackTarget::Player(P1)))
        .collect();
    b.runner
        .declare_attackers(&attacks)
        .expect("attack with two creatures");
    assert_eq!(observer_triggers(&b.runner, b.observer), 1);

    let first = b.attackers[0];
    bounce_attacker(&mut b, first);
    b.runner.advance_until_stack_empty();
    assert_eq!(b.runner.state().objects[&b.draw].zone, Zone::Hand);
}

/// CR 506.4: an attacker removed from combat and then returned to hand was
/// attacking nothing when it left, so it no longer counts.
#[test]
fn mangara_does_not_count_an_attacker_removed_from_combat_before_it_left() {
    let mut b = board(2, P1, "Mangara, the Diplomat", MANGARA, 2);
    let attacks: Vec<_> = b
        .attackers
        .iter()
        .map(|&id| (id, AttackTarget::Player(P1)))
        .collect();
    b.runner
        .declare_attackers(&attacks)
        .expect("attack with two creatures");
    assert_eq!(observer_triggers(&b.runner, b.observer), 1);

    let first = b.attackers[0];
    steal_attacker(&mut b, first);
    bounce_attacker(&mut b, first);
    b.runner.advance_until_stack_empty();
    assert_eq!(b.runner.state().objects[&b.draw].zone, Zone::Library);
}

#[test]
fn mangara_rechecks_live_attackers_after_a_control_change() {
    let mut b = board(2, P1, "Mangara, the Diplomat", MANGARA, 2);
    b.runner
        .declare_attackers(&[
            (b.attackers[0], AttackTarget::Player(P1)),
            (b.attackers[1], AttackTarget::Planeswalker(b.planeswalker)),
        ])
        .expect("attack you and your planeswalker");
    assert_eq!(observer_triggers(&b.runner, b.observer), 1);

    let first = b.attackers[0];
    steal_attacker(&mut b, first);
    b.runner.advance_until_stack_empty();

    assert_eq!(b.runner.state().objects[&b.draw].zone, Zone::Library);
    assert!(b.runner.state().stack.is_empty());
    assert!(matches!(
        b.runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn mangara_rechecks_the_departed_attackers_original_incarnation_after_restore() {
    let mut b = board(2, P1, "Mangara, the Diplomat", MANGARA, 2);
    let attacker = b.attackers[1];
    b.runner
        .declare_attackers(&[
            (b.attackers[0], AttackTarget::Player(P1)),
            (attacker, AttackTarget::Planeswalker(b.planeswalker)),
        ])
        .expect("attack you and your planeswalker");
    assert_eq!(observer_triggers(&b.runner, b.observer), 1);
    let incarnation = b.runner.state().objects[&attacker].incarnation;
    let mut events = Vec::new();
    move_to_zone(b.runner.state_mut(), attacker, Zone::Exile, &mut events);
    move_to_zone(
        b.runner.state_mut(),
        attacker,
        Zone::Battlefield,
        &mut events,
    );
    assert_ne!(b.runner.state().objects[&attacker].incarnation, incarnation);
    assert!(!b
        .runner
        .state()
        .combat
        .as_ref()
        .unwrap()
        .attackers
        .iter()
        .any(|info| info.object_id == attacker));

    let departure = b
        .runner
        .state()
        .zone_changes_this_turn
        .iter()
        .position(|row| row.object_id == attacker && row.from_zone == Some(Zone::Battlefield))
        .expect("the attacking incarnation has a departure row");

    let state = b.runner.state().clone();
    for (save, wrapped) in [
        (PersistedGameState::Raw(Box::new(state.clone())), false),
        (PersistedGameState::capture(state.clone()), true),
    ] {
        let saved = serde_json::to_value(save).expect("the game serializes");
        let restored = serde_json::from_value::<PersistedGameState>(saved.clone())
            .expect("the persisted game decodes")
            .into_game_state()
            .expect("the exact departure evidence survives checked restore");
        assert_mangara_draws(GameRunner::from_state(restored), b.draw);

        let mut duplicate_attacker = saved.clone();
        let payload = if wrapped {
            &mut duplicate_attacker["state"]
        } else {
            &mut duplicate_attacker
        };
        let attacker_ids = payload["stack"][0]["kind"]["data"]["trigger_event"]["data"]
            ["attacker_ids"]
            .as_array_mut()
            .expect("the trigger carries the declared attacker IDs");
        attacker_ids.push(serde_json::to_value(attacker).unwrap());
        assert!(
            serde_json::from_value::<PersistedGameState>(duplicate_attacker)
                .expect_err("a declared attacker can only count once in the recheck")
                .to_string()
                .contains("exact attack authority")
        );

        for missing_target in [false, true] {
            let mut broken = saved.clone();
            let payload = if wrapped {
                &mut broken["state"]
            } else {
                &mut broken
            };
            let status = payload["zone_changes_this_turn"][departure]["combat_status"]
                .as_object_mut()
                .expect("the departure has combat status");
            assert!(status.remove("attack_target_identity").is_some());
            if missing_target {
                assert!(status.remove("attack_target").is_some());
            }
            assert!(serde_json::from_value::<PersistedGameState>(broken)
                .expect_err("the pending recheck requires exact departure authority")
                .to_string()
                .contains("exact attack authority"));
        }

        let mut missing_attacker_pin = saved.clone();
        let payload = if wrapped {
            &mut missing_attacker_pin["state"]
        } else {
            &mut missing_attacker_pin
        };
        let records = payload["stack"][0]["kind"]["data"]["trigger_event"]["data"]
            ["declaration_records"]
            .as_array_mut()
            .expect("the trigger carries its declaration");
        let attacker_id = serde_json::to_value(attacker).unwrap();
        let record = records
            .iter_mut()
            .find(|record| record["object_id"] == attacker_id)
            .expect("the declaration names the departed attacker");
        assert!(record
            .as_object_mut()
            .unwrap()
            .remove("incarnation")
            .is_some());
        assert!(
            serde_json::from_value::<PersistedGameState>(missing_attacker_pin)
                .expect_err("a pending recheck must name the declared incarnation")
                .to_string()
                .contains("exact attack authority")
        );

        let mut broken = saved;
        let payload = if wrapped {
            &mut broken["state"]
        } else {
            &mut broken
        };
        let pin = &mut payload["zone_changes_this_turn"][departure]["combat_status"]
            ["attack_target_identity"];
        let incarnation = pin["incarnation"]
            .as_u64()
            .expect("the destination is pinned");
        pin["incarnation"] = serde_json::json!(incarnation + 1);
        assert!(serde_json::from_value::<PersistedGameState>(broken)
            .expect_err("a same-id pin with the wrong incarnation has no authority")
            .to_string()
            .contains("exact attack authority"));
    }
    assert_mangara_draws(b.runner, b.draw);
}

fn assert_mangara_draws(mut runner: GameRunner, draw: ObjectId) {
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&draw].zone, Zone::Hand);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn mangara_stops_counting_an_attacker_whose_planeswalker_leaves_combat() {
    for attacker_leaves in [false, true] {
        let mut b = board(2, P1, "Mangara, the Diplomat", MANGARA, 2);
        b.runner
            .declare_attackers(&[
                (b.attackers[0], AttackTarget::Player(P1)),
                (b.attackers[1], AttackTarget::Planeswalker(b.planeswalker)),
            ])
            .expect("attack you and your planeswalker");
        assert_eq!(observer_triggers(&b.runner, b.observer), 1);

        move_to_zone(
            b.runner.state_mut(),
            b.planeswalker,
            Zone::Graveyard,
            &mut Vec::new(),
        );
        assert!(b
            .runner
            .state()
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .any(|info| info.object_id == b.attackers[1]));

        if attacker_leaves {
            let attacker = b.attackers[1];
            move_to_zone(
                b.runner.state_mut(),
                attacker,
                Zone::Graveyard,
                &mut Vec::new(),
            );
            let departure = b
                .runner
                .state()
                .zone_changes_this_turn
                .iter()
                .find(|row| row.object_id == attacker && row.from_zone == Some(Zone::Battlefield))
                .expect("the attacker has a departure row");
            assert!(departure.combat_status.attacking);
            assert_eq!(departure.combat_status.attack_target, None);
            let saved = serde_json::to_value(PersistedGameState::capture(b.runner.state().clone()))
                .expect("the game serializes");
            let restored = serde_json::from_value::<PersistedGameState>(saved)
                .expect("an explicit empty destination survives restore")
                .into_game_state()
                .expect("the game restores");
            b.runner = GameRunner::from_state(restored);
        }

        b.runner.advance_until_stack_empty();
        assert_eq!(b.runner.state().objects[&b.draw].zone, Zone::Library);
        assert!(b.runner.state().stack.is_empty());
        assert!(matches!(
            b.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

#[test]
fn mangara_counts_a_departed_attacker_even_if_its_planeswalker_leaves_later() {
    let mut b = board(2, P1, "Mangara, the Diplomat", MANGARA, 2);
    let attacker = b.attackers[1];
    b.runner
        .declare_attackers(&[
            (b.attackers[0], AttackTarget::Player(P1)),
            (attacker, AttackTarget::Planeswalker(b.planeswalker)),
        ])
        .expect("attack you and your planeswalker");
    assert_eq!(observer_triggers(&b.runner, b.observer), 1);

    let mut events = Vec::new();
    move_to_zone(b.runner.state_mut(), attacker, Zone::Graveyard, &mut events);
    move_to_zone(
        b.runner.state_mut(),
        b.planeswalker,
        Zone::Graveyard,
        &mut events,
    );
    let saved = serde_json::to_value(PersistedGameState::capture(b.runner.state().clone()))
        .expect("the game serializes");
    let restored = serde_json::from_value::<PersistedGameState>(saved)
        .expect("the original planeswalker's departure is available")
        .into_game_state()
        .expect("the game restores");
    assert_mangara_draws(GameRunner::from_state(restored), b.draw);
}

#[test]
fn mangara_rechecks_a_reselected_destination_without_counting_a_new_attacker() {
    let mut b = board(3, P1, "Mangara, the Diplomat", MANGARA, 3);
    let [first, redirected, late] = b.attackers[..] else {
        panic!("the fixture has three creatures");
    };
    b.runner
        .declare_attackers(&[
            (first, AttackTarget::Player(P1)),
            (redirected, AttackTarget::Player(P1)),
        ])
        .expect("exactly two creatures are declared at Mangara's controller");
    assert_eq!(observer_triggers(&b.runner, b.observer), 1);

    // Supply the settled combat assignment directly. This is a recheck test,
    // independent of support for a printed reselection effect.
    let other = PlayerId(2);
    let combat = b.runner.state_mut().combat.as_mut().unwrap();
    let attack = combat
        .attackers
        .iter_mut()
        .find(|info| info.object_id == redirected)
        .expect("the declared attacker is still in combat");
    attack.attack_target = AttackTarget::Player(other);
    attack.defending_player = other;
    engine::game::combat::enter_attacking_at_target(
        b.runner.state_mut(),
        late,
        P1,
        AttackTarget::Player(P1),
    );
    assert_eq!(b.runner.state().combat.as_ref().unwrap().attackers.len(), 3);

    b.runner.advance_until_stack_empty();
    assert_eq!(b.runner.state().objects[&b.draw].zone, Zone::Library);
    assert!(b.runner.state().stack.is_empty());
    assert!(matches!(
        b.runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

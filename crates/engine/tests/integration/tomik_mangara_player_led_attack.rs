//! Tomik, Wielder of Law and Mangara, the Diplomat: "Whenever an opponent
//! attacks with creatures, if two or more of those creatures are attacking you
//! and/or planeswalkers you control, ..." fires once for the attacking
//! opponent's declaration (CR 508.3c), counts that opponent's whole attack, and
//! Tomik's "that opponent" is the attacking player.

use engine::game::combat::AttackTarget;
use engine::game::merge::{merge_object_onto, MergeSide};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::move_to_zone;
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, PersistedGameState, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::{parse_mtgjson_mana_cost, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::Zone;

use crate::support::shared_card_db;

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

/// CR 506.4c + CR 608.2h: Mangara counts the attack at Jace when both
/// Jace and its attacker left in the same event.
#[test]
fn mangara_counts_an_attacker_destroyed_with_its_attack_target() {
    for (planeswalker_first, merged_attacker) in [(true, false), (false, false), (true, true)] {
        assert_simultaneous_attack_departures(planeswalker_first, merged_attacker);
    }
}

/// CR 603.10a + CR 608.2h: Fated Retribution destroys Jace and the Bear
/// attacking it in one event; in either battlefield order, and across a
/// checked save/restore, Mangara still sees two qualifying attackers.
#[test]
fn mangara_draws_once_after_fated_retribution_destroys_jace_and_its_attacker() {
    for (jace_first, round_trip) in [(true, false), (false, false), (true, true)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let observer = scenario
            .add_creature_from_oracle(P1, "Mangara, the Diplomat", 2, 4, MANGARA)
            .id();
        let add_jace = |scenario: &mut GameScenario| {
            scenario
                .add_planeswalker_from_oracle(P1, "Jace Beleren", "Jace", 3, JACE_BELEREN)
                .id()
        };
        let add_bear =
            |scenario: &mut GameScenario| scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let (jace, at_jace) = if jace_first {
            let jace = add_jace(&mut scenario);
            (jace, add_bear(&mut scenario))
        } else {
            let bear = add_bear(&mut scenario);
            (add_jace(&mut scenario), bear)
        };
        let at_player = add_bear(&mut scenario);
        let wipe = scenario
            .add_spell_to_hand_from_oracle(
                P1,
                "Fated Retribution",
                true,
                "Destroy all creatures and planeswalkers. If it's your turn, scry 2.",
            )
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let draw = scenario.add_card_to_library_top(P1, "Island");
        let mut runner = scenario.build();
        runner.advance_to_combat();
        runner
            .declare_attackers(&[
                (at_jace, AttackTarget::Planeswalker(jace)),
                (at_player, AttackTarget::Player(P1)),
            ])
            .expect("attack you and your planeswalker");
        assert_eq!(observer_triggers(&runner, observer), 1);

        runner
            .act(GameAction::PassPriority)
            .expect("the attacking player passes");
        runner.cast(wipe).commit();
        runner.resolve_top();
        for id in [jace, at_jace, at_player] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
        }
        assert_eq!(observer_triggers(&runner, observer), 1);

        if round_trip {
            let saved = serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
                .expect("the pending trigger saves");
            let restored = serde_json::from_value::<PersistedGameState>(saved)
                .expect("the pending trigger decodes")
                .into_game_state()
                .expect("the simultaneous departure evidence restores");
            runner = GameRunner::from_state(restored);
        }
        assert_mangara_draws(runner, draw);
    }
}

fn assert_simultaneous_attack_departures(planeswalker_first: bool, merged_attacker: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let observer = scenario
        .add_creature_from_oracle(P1, "Mangara, the Diplomat", 2, 4, MANGARA)
        .id();
    let add_jace = |scenario: &mut GameScenario| {
        scenario
            .add_planeswalker_from_oracle(P1, "Jace Beleren", "Jace", 3, JACE_BELEREN)
            .id()
    };
    let add_attacker =
        |scenario: &mut GameScenario| scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let (jace, at_jace) = if planeswalker_first {
        (add_jace(&mut scenario), add_attacker(&mut scenario))
    } else {
        let attacker = add_attacker(&mut scenario);
        (add_jace(&mut scenario), attacker)
    };
    let component = merged_attacker.then(|| scenario.add_creature(P0, "Gemrazer", 4, 4).id());
    let at_player = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let stone = scenario
        .add_artifact_from_oracle(
            P0,
            "Oblivion Stone",
            "{4}, {T}: Put a fate counter on target permanent.\n{5}, {T}, Sacrifice this artifact: Destroy each nonland permanent without a fate counter on it, then remove all fate counters from all permanents.",
        )
        .id();
    let draw = scenario.add_card_to_library_top(P1, "Island");
    let mut runner = scenario.build();
    if let Some(component) = component {
        // Set up the merged permanent through the same helper used by mutate.
        let state = runner.state_mut();
        merge_object_onto(
            state,
            component,
            at_jace,
            MergeSide::Bottom,
            &mut Vec::new(),
        );
        state.battlefield.retain(|&id| id != component);
        assert_eq!(
            state.objects[&at_jace].merged_components,
            [at_jace, component]
        );
    }
    let jace_identity = ObjectIncarnationRef::of(jace, runner.state().objects[&jace].incarnation);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (at_jace, AttackTarget::Planeswalker(jace)),
            (at_player, AttackTarget::Player(P1)),
        ])
        .expect("attack you and your planeswalker");
    assert_eq!(observer_triggers(&runner, observer), 1);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|player| player.id == P0)
        .expect("active player")
        .mana_pool;
    for _ in 0..5 {
        pool.add(ManaUnit::new(ManaType::Colorless, stone, false, vec![]));
    }
    runner
        .act(GameAction::ActivateAbility {
            source_id: stone,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Oblivion Stone in response to Mangara");
    assert_eq!(runner.state().stack.last().unwrap().source_id, stone);
    runner.resolve_top();
    assert_eq!(observer_triggers(&runner, observer), 1);
    let records = &runner.state().zone_changes_this_turn;
    let departure = |id| {
        records
            .iter()
            .position(|record| {
                record.object_id == id && record.from_zone == Some(Zone::Battlefield)
            })
            .expect("Oblivion Stone destroyed the permanent")
    };
    let attacker_record = departure(at_jace);
    assert_eq!(departure(jace) < attacker_record, planeswalker_first);
    for attacker in [at_jace, at_player] {
        let status = records[departure(attacker)].combat_status;
        assert!(status.attacking && !status.attacking_alone);
    }
    if let Some(component) = component {
        assert_eq!(runner.state().objects[&component].zone, Zone::Graveyard);
    }
    assert_eq!(
        records[attacker_record].combat_status.attack_target,
        Some(AttackTarget::Planeswalker(jace))
    );
    assert_eq!(
        records[attacker_record]
            .combat_status
            .attack_target_identity,
        Some(jace_identity)
    );
    assert_mangara_draws(runner, draw);
}

/// CR 506.4c + CR 608.2h: Displacement Wave removes Jace, pauses on an
/// absorbed commander's hand replacement, then removes Jace's attacker.
#[test]
fn mangara_keeps_the_pre_departure_attack_target_across_a_parked_batch() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let jace = scenario.add_real_card(P1, "Jace Beleren", Zone::Battlefield, db);
    let at_player = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let at_jace = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let observer = scenario.add_real_card(P1, "Mangara, the Diplomat", Zone::Battlefield, db);
    let commander = scenario.add_real_card(P0, "Brokkos, Apex of Forever", Zone::Command, db);
    let orrery = scenario.add_real_card(P0, "Vedalken Orrery", Zone::Battlefield, db);
    let wave = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Displacement Wave",
            false,
            "Return all nonland permanents with mana value X or less to their owners' hands.",
        )
        .with_mana_cost(parse_mtgjson_mana_cost("{X}{U}{U}"))
        .id();
    let draw = scenario.add_card_to_library_top(P1, "Island");
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.objects.get_mut(&commander).unwrap().is_commander = true;
    merge_object_onto(
        state,
        commander,
        at_player,
        MergeSide::Bottom,
        &mut Vec::new(),
    );
    assert_eq!(
        state.objects[&at_player].merged_components,
        [at_player, commander]
    );

    let jace_identity = ObjectIncarnationRef::of(jace, runner.state().objects[&jace].incarnation);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (at_player, AttackTarget::Player(P1)),
            (at_jace, AttackTarget::Planeswalker(jace)),
        ])
        .expect("attack Mangara's controller and their planeswalker");
    assert_eq!(observer_triggers(&runner, observer), 1);
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for _ in 0..6 {
        pool.add(ManaUnit::new(ManaType::Blue, orrery, false, vec![]));
    }
    runner.cast(wave).x(4).commit();
    runner.resolve_top();
    let WaitingFor::ReplacementChoice {
        player: P0,
        candidates,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the merged commander's replacement, got {:?}",
            runner.state().waiting_for
        );
    };
    let command = candidates
        .iter()
        .position(|choice| choice.description == "Move to command zone")
        .expect("the owner can choose the command zone");
    assert_eq!(
        runner
            .state()
            .pending_replacement
            .as_ref()
            .and_then(|pending| pending.proposed.affected_object_id()),
        Some(commander)
    );
    assert_eq!(runner.state().objects[&jace].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&at_jace].zone, Zone::Battlefield);
    let combat = runner
        .state()
        .combat
        .as_ref()
        .expect("combat is still in progress");
    assert!(!combat.attacked_permanents.contains_key(&at_jace));

    let saved = serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
        .expect("the interrupted resolution saves");
    let restored = serde_json::from_value::<PersistedGameState>(saved)
        .expect("the interrupted resolution decodes")
        .into_game_state()
        .expect("the interrupted resolution restores");
    let mut runner = GameRunner::from_state(restored);
    runner
        .act(GameAction::ChooseReplacement { index: command })
        .expect("the owner moves their commander to the command zone");
    assert_eq!(runner.state().objects[&commander].zone, Zone::Command);
    assert_eq!(runner.state().objects[&at_jace].zone, Zone::Hand);
    assert_eq!(observer_triggers(&runner, observer), 1);
    let departure = runner
        .state()
        .zone_changes_this_turn
        .iter()
        .find(|record| record.object_id == at_jace && record.from_zone == Some(Zone::Battlefield))
        .expect("the attacker departed after restore");
    assert_eq!(
        departure.combat_status.attack_target,
        Some(AttackTarget::Planeswalker(jace))
    );
    assert_eq!(
        departure.combat_status.attack_target_identity,
        Some(jace_identity)
    );
    assert_mangara_draws(runner, draw);
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

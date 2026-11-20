//! Runtime witnesses for the `confirmed-misparses.toml` rows released in batch 5: three cards whose
//! "until end of turn" clause the census marked `wrong_object`, but whose clause the engine already
//! applies to the right object. Every released card is the printed card (printed cost, type line,
//! P/T and verbatim Oracle text, loaded from the card database through `add_real_card`) and is
//! driven through the public cast / activate / attack actions of `GameRunner`. Only textless filler
//! creatures are synthetic.
//!
//! The shapes, one section each: "whenever one or more opponents are dealt noncombat damage,
//! creatures you control get +1/+0 until end of turn" (Master of Barbs), "you may have this
//! creature's base power become equal to that creature's power until end of turn" (Belligerent
//! Yearling) and "search your library for a Dragon permanent card and put it into your graveyard; if
//! you do, <this> becomes a copy of that card until end of turn" (Scion of the Ur-Dragon). Every
//! positive has a control that must stay unaffected, and every effect is shown to end with the turn
//! (CR 514.2, 611.2a).
use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::AbilityKind;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn db() -> Option<&'static CardDatabase> {
    shared_card_db()
}

/// Mana as pool mana (CR 106.4) so the cast and activation pipelines pay exactly the printed cost.
fn pool(kinds: &[(ManaType, usize)]) -> Vec<ManaUnit> {
    kinds
        .iter()
        .flat_map(|&(kind, count)| std::iter::repeat_n(kind, count))
        .enumerate()
        .map(|(i, kind)| ManaUnit::new(kind, ObjectId(9900 + i as u64), false, vec![]))
        .collect()
}

/// A main-phase scenario with `players` seats and a few draws in each library, so playing the turn
/// structure out never decks anyone.
fn scenario(players: u8) -> GameScenario {
    let mut scenario = GameScenario::new_n_player(players, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Draw 1", "Draw 2", "Draw 3", "Draw 4"]);
    }
    scenario
}

/// Answer every prompt with the plain default (pass priority, no attacks, no blocks) until `done`
/// holds.
fn drive_until(runner: &mut GameRunner, label: &str, done: impl Fn(&GameRunner) -> bool) {
    for _ in 0..400 {
        if done(runner) {
            return;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            ref other => panic!("{label}: unexpected prompt {other:?}"),
        };
        runner.act(action).expect(label);
    }
    panic!("{label}: never reached");
}

/// Play on to this turn's end step (CR 513): an effect that lasts "until end of turn" is still on
/// there (it is not an until-end-of-combat effect) and is gone after the cleanup step (CR 514.2).
fn to_end_step(runner: &mut GameRunner) {
    drive_until(runner, "end step", |r| {
        r.state().phase == Phase::End
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
    });
}

/// Play the rest of this turn and the opponent's turn out, and stop at P0's next declare-attackers
/// prompt: every "until end of turn" effect has expired by then (CR 514.2).
fn to_next_turn(runner: &mut GameRunner) {
    let turn = runner.state().turn_number;
    drive_until(runner, "next own turn", |r| {
        r.state().turn_number > turn + 1
            && matches!(
                r.state().waiting_for,
                WaitingFor::DeclareAttackers { player: P0, .. }
            )
    });
}

fn power_toughness(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let o = &runner.state().objects[&id];
    (o.power.unwrap_or(0), o.toughness.unwrap_or(0))
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn ability_index_with_description(runner: &GameRunner, source: ObjectId, fragment: &str) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| {
            ability.kind == AbilityKind::Activated
                && ability
                    .description
                    .as_deref()
                    .is_some_and(|description| description.contains(fragment))
        })
        .unwrap_or_else(|| panic!("no activated ability whose text contains {fragment:?}"))
}

// ---- Shape 1: "Whenever one or more opponents are dealt noncombat damage, creatures you control get
// +1/+0 until end of turn." (Master of Barbs). ----

/// One noncombat damage event to an opponent (Shock, printed) pumps every creature P0 controls once,
/// by +1/+0 and no more, leaves the opponent's creatures alone, skips creatures that enter after the
/// trigger resolved (CR 611.2c) and ends with the turn.
#[test]
fn master_of_barbs_pumps_creatures_you_control_when_an_opponent_is_dealt_noncombat_damage() {
    let Some(db) = db() else { return };
    let mut s = scenario(2);
    let master = s.add_real_card(P0, "Master of Barbs", Zone::Battlefield, db);
    let ally = s.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let enemy = s.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let late = s.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    let shock = s.add_real_card(P0, "Shock", Zone::Hand, db);
    s.with_mana_pool(
        P0,
        pool(&[
            (ManaType::Red, 1),
            (ManaType::Green, 1),
            (ManaType::Colorless, 1),
        ]),
    );
    let mut runner = s.build();
    assert_eq!(power_toughness(&runner, master), (2, 1), "printed 2/1");

    runner.cast(shock).target_player(P1).resolve();
    // Reach-guard: Shock really dealt its 2 noncombat damage to the opponent (CR 120.3a).
    assert_eq!(life(&runner, P1), 18, "Shock dealt 2 damage to P1");
    assert_eq!(
        power_toughness(&runner, master),
        (3, 1),
        "one trigger: Master of Barbs gets +1/+0 and its toughness is unchanged"
    );
    assert_eq!(
        power_toughness(&runner, ally),
        (3, 2),
        "the other creature P0 controls gets +1/+0 too"
    );
    assert_eq!(
        power_toughness(&runner, enemy),
        (2, 2),
        "a creature the opponent controls is not pumped"
    );

    runner.cast(late).resolve();
    assert_eq!(
        power_toughness(&runner, late),
        (2, 2),
        "a creature that enters after the trigger resolved is not pumped (CR 611.2c)"
    );

    to_end_step(&mut runner);
    assert_eq!(
        power_toughness(&runner, master),
        (3, 1),
        "the +1/+0 lasts to the end step (until end of turn, not until end of combat)"
    );
    to_next_turn(&mut runner);
    assert_eq!(
        power_toughness(&runner, master),
        (2, 1),
        "the +1/+0 ends with the turn"
    );
    assert_eq!(power_toughness(&runner, ally), (2, 2), "so does the ally's");
}

/// Controls: the trigger needs an OPPONENT dealt NONCOMBAT damage. Damage to P0 itself, damage to
/// an opponent's creature and combat damage to the opponent all leave the board unpumped.
#[test]
fn master_of_barbs_ignores_damage_that_is_not_noncombat_damage_to_an_opponent() {
    let Some(db) = db() else { return };

    // Shock at its own controller: the controller is not an opponent.
    let mut s = scenario(2);
    let master = s.add_real_card(P0, "Master of Barbs", Zone::Battlefield, db);
    let shock = s.add_real_card(P0, "Shock", Zone::Hand, db);
    s.with_mana_pool(P0, pool(&[(ManaType::Red, 1)]));
    let mut runner = s.build();
    runner.cast(shock).target_player(P0).resolve();
    assert_eq!(life(&runner, P0), 18, "reach-guard: P0 took the damage");
    assert_eq!(
        power_toughness(&runner, master),
        (2, 1),
        "damage to the controller does not trigger it"
    );

    // Shock at an opponent's creature: a creature is not an opponent.
    let mut s = scenario(2);
    let master = s.add_real_card(P0, "Master of Barbs", Zone::Battlefield, db);
    let enemy = s.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let shock = s.add_real_card(P0, "Shock", Zone::Hand, db);
    s.with_mana_pool(P0, pool(&[(ManaType::Red, 1)]));
    let mut runner = s.build();
    runner.cast(shock).target_object(enemy).resolve();
    assert_eq!(
        runner.state().objects[&enemy].zone,
        Zone::Graveyard,
        "reach-guard: Shock killed the opponent's creature"
    );
    assert_eq!(
        life(&runner, P1),
        20,
        "the opponent itself was not dealt damage"
    );
    assert_eq!(
        power_toughness(&runner, master),
        (2, 1),
        "damage to an opponent's creature does not trigger it"
    );

    // Combat damage to the opponent: the clause says noncombat.
    let mut s = scenario(2);
    let master = s.add_real_card(P0, "Master of Barbs", Zone::Battlefield, db);
    let attacker = s.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = s.build();
    drive_until(&mut runner, "declare attackers", |r| {
        matches!(
            r.state().waiting_for,
            WaitingFor::DeclareAttackers { player: P0, .. }
        )
    });
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    drive_until(&mut runner, "combat damage dealt", |r| life(r, P1) < 20);
    assert_eq!(life(&runner, P1), 18, "reach-guard: the Bears hit P1 for 2");
    assert_eq!(
        power_toughness(&runner, master),
        (2, 1),
        "combat damage does not trigger it"
    );
}

/// "One or more opponents": Goblin Chainwhirler (printed) deals 1 damage to each opponent at once,
/// so with two opponents the one damage event is one trigger, +1/+0 and not +2/+0.
#[test]
fn master_of_barbs_triggers_once_for_one_damage_event_to_several_opponents() {
    let Some(db) = db() else { return };
    let mut s = scenario(3);
    let master = s.add_real_card(P0, "Master of Barbs", Zone::Battlefield, db);
    let whirler = s.add_real_card(P0, "Goblin Chainwhirler", Zone::Hand, db);
    s.with_mana_pool(P0, pool(&[(ManaType::Red, 3)]));
    let mut runner = s.build();
    runner.cast(whirler).resolve();
    assert_eq!(
        runner.state().objects[&whirler].zone,
        Zone::Battlefield,
        "reach-guard: Goblin Chainwhirler entered"
    );
    assert_eq!(life(&runner, P1), 19, "reach-guard: P1 was dealt 1 damage");
    assert_eq!(
        life(&runner, PlayerId(2)),
        19,
        "reach-guard: P2 was dealt 1 damage"
    );
    assert_eq!(
        power_toughness(&runner, master),
        (3, 1),
        "one damage event to both opponents is one trigger"
    );
}

// ---- Shape 2: "Whenever another Dinosaur you control enters, you may have this creature's base
// power become equal to that creature's power until end of turn." (Belligerent Yearling). ----

/// P0's Yearling on the battlefield and the Dinosaur (or other creature) `enterer` in P0's hand.
fn yearling_board(db: &CardDatabase, enterer: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut s = scenario(2);
    let yearling = s.add_real_card(P0, "Belligerent Yearling", Zone::Battlefield, db);
    let enters = s.add_real_card(P0, enterer, Zone::Hand, db);
    s.with_mana_pool(P0, pool(&[(ManaType::Green, 3), (ManaType::Colorless, 4)]));
    (s.build(), yearling, enters)
}

/// Accepting sets the Yearling's base power to the entering Dinosaur's power (6 from Colossal
/// Dreadmaw, 2 from Ranging Raptors, so it is "equal to", not "at least"), leaves its toughness at
/// the printed 2, composes with a pump (base 6 plus Giant Growth's +3 is 9, so it is the BASE power)
/// and ends with the turn.
#[test]
fn belligerent_yearling_base_power_becomes_the_entering_dinosaurs_power() {
    let Some(db) = db() else { return };

    let (mut runner, yearling, dreadmaw) = yearling_board(db, "Colossal Dreadmaw");
    assert_eq!(power_toughness(&runner, yearling), (3, 2), "printed 3/2");
    runner.cast(dreadmaw).accept_optional().resolve();
    assert_eq!(
        runner.state().objects[&dreadmaw].zone,
        Zone::Battlefield,
        "reach-guard: the Dinosaur entered"
    );
    assert_eq!(
        power_toughness(&runner, yearling),
        (6, 2),
        "base power becomes 6; toughness stays 2"
    );
    to_end_step(&mut runner);
    assert_eq!(
        power_toughness(&runner, yearling),
        (6, 2),
        "the base power change lasts to the end step"
    );
    to_next_turn(&mut runner);
    assert_eq!(
        power_toughness(&runner, yearling),
        (3, 2),
        "the base power change ends with the turn"
    );

    let (mut runner, yearling, raptors) = yearling_board(db, "Ranging Raptors");
    runner.cast(raptors).accept_optional().resolve();
    assert_eq!(
        runner.state().objects[&raptors].zone,
        Zone::Battlefield,
        "reach-guard: the Dinosaur entered"
    );
    assert_eq!(
        power_toughness(&runner, yearling),
        (2, 2),
        "a smaller Dinosaur lowers the base power: equal to, not at least"
    );

    // Base power, not a final value: a pump cast afterwards stacks on top of the new base.
    let mut s = scenario(2);
    let yearling = s.add_real_card(P0, "Belligerent Yearling", Zone::Battlefield, db);
    let dreadmaw = s.add_real_card(P0, "Colossal Dreadmaw", Zone::Hand, db);
    let growth = s.add_real_card(P0, "Giant Growth", Zone::Hand, db);
    s.with_mana_pool(P0, pool(&[(ManaType::Green, 3), (ManaType::Colorless, 4)]));
    let mut runner = s.build();
    runner.cast(dreadmaw).accept_optional().resolve();
    runner.cast(growth).target_object(yearling).resolve();
    assert_eq!(
        power_toughness(&runner, yearling),
        (9, 5),
        "base 6 plus Giant Growth's +3/+3"
    );
}

/// Controls: declining ("you may") changes nothing; a non-Dinosaur entering and a Dinosaur entering
/// under the opponent's control do not trigger it.
#[test]
fn belligerent_yearling_ignores_declines_other_creature_types_and_other_controllers() {
    let Some(db) = db() else { return };

    let (mut runner, yearling, dreadmaw) = yearling_board(db, "Colossal Dreadmaw");
    runner.cast(dreadmaw).decline_optional().resolve();
    assert_eq!(
        runner.state().objects[&dreadmaw].zone,
        Zone::Battlefield,
        "reach-guard: the Dinosaur entered"
    );
    assert_eq!(
        power_toughness(&runner, yearling),
        (3, 2),
        "declining the optional change leaves the base power alone"
    );

    // Not a Dinosaur: Grizzly Bears (printed 2/2 Bear) does not trigger it even when it is accepted.
    let (mut runner, yearling, bears) = yearling_board(db, "Grizzly Bears");
    runner.cast(bears).accept_optional().resolve();
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Battlefield,
        "reach-guard: the Bears entered"
    );
    assert_eq!(
        power_toughness(&runner, yearling),
        (3, 2),
        "a Bear is not a Dinosaur"
    );

    // Another player's Dinosaur: P1 casts Colossal Dreadmaw on its own turn, so it is not "you control".
    let mut s = scenario(2);
    let yearling = s.add_real_card(P0, "Belligerent Yearling", Zone::Battlefield, db);
    let dreadmaw = s.add_real_card(P1, "Colossal Dreadmaw", Zone::Hand, db);
    let mut runner = s.build();
    drive_until(&mut runner, "P1's first main phase", |r| {
        r.state().phase == Phase::PreCombatMain
            && r.state().active_player == P1
            && matches!(r.state().waiting_for, WaitingFor::Priority { player: P1 })
    });
    for unit in pool(&[(ManaType::Green, 2), (ManaType::Colorless, 4)]) {
        runner.state_mut().players[1].mana_pool.add(unit);
    }
    runner.cast(dreadmaw).accept_optional().resolve();
    assert_eq!(
        runner.state().objects[&dreadmaw].zone,
        Zone::Battlefield,
        "reach-guard: the opponent's Dinosaur entered"
    );
    assert_eq!(
        power_toughness(&runner, yearling),
        (3, 2),
        "an opponent's Dinosaur does not trigger it"
    );
}

// ---- Shape 3: "{2}: Search your library for a Dragon permanent card and put it into your
// graveyard. If you do, <this> becomes a copy of that card until end of turn." (Scion of the
// Ur-Dragon). ----

/// The Scion searches for a Dragon permanent only (a Bear in the library is not offered), puts the
/// card into the graveyard, becomes a copy of that Dragon (name, printed 5/5, Flying, its {R}
/// firebreathing, and no longer its own search ability, CR 707.2) and is the Scion again next turn.
#[test]
fn scion_of_the_ur_dragon_becomes_a_copy_of_the_dragon_it_put_into_the_graveyard() {
    let Some(db) = db() else { return };
    let mut s = scenario(2);
    let scion = s.add_real_card(P0, "Scion of the Ur-Dragon", Zone::Battlefield, db);
    let bears = s.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    let shivan = s.add_real_card(P0, "Shivan Dragon", Zone::Library, db);
    s.with_mana_pool(P0, pool(&[(ManaType::Colorless, 2), (ManaType::Red, 1)]));
    let mut runner = s.build();
    assert_eq!(power_toughness(&runner, scion), (4, 4), "printed 4/4");

    let search = ability_index_with_description(&runner, scion, "Search your library");
    runner
        .activate(scion, search)
        .search_first_legal()
        .resolve();

    assert_eq!(
        runner.state().objects[&shivan].zone,
        Zone::Graveyard,
        "the Dragon permanent card was put into the graveyard"
    );
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Library,
        "the Bear was not a legal search result"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        1,
        "the {{2}} activation cost was paid from the pool, the red mana is left"
    );
    let copy = &runner.state().objects[&scion];
    assert_eq!(copy.name, "Shivan Dragon", "it became a copy of that card");
    assert_eq!(
        power_toughness(&runner, scion),
        (5, 5),
        "copiable P/T (CR 707.2)"
    );
    assert!(copy.has_keyword(&Keyword::Flying), "the copy has Flying");
    assert!(
        copy.abilities.iter().all(|ability| ability
            .description
            .as_deref()
            .is_none_or(|d| !d.contains("Search your library"))),
        "the copy has the copied abilities, not the Scion's own"
    );
    let firebreathing = ability_index_with_description(&runner, scion, "+1/+0");
    runner.activate(scion, firebreathing).resolve();
    assert_eq!(
        power_toughness(&runner, scion),
        (6, 5),
        "the copy carries Shivan Dragon's {{R}}: +1/+0 and it works"
    );

    to_end_step(&mut runner);
    assert_eq!(
        runner.state().objects[&scion].name,
        "Shivan Dragon",
        "the copy lasts to the end step"
    );
    to_next_turn(&mut runner);
    let back = &runner.state().objects[&scion];
    assert_eq!(
        back.name, "Scion of the Ur-Dragon",
        "the copy ends with the turn"
    );
    assert_eq!(power_toughness(&runner, scion), (4, 4), "printed 4/4 again");
    ability_index_with_description(&runner, scion, "Search your library");
}

/// Control for "if you do": with no Dragon permanent card in the library nothing is put into the
/// graveyard and the Scion does not become a copy of anything.
#[test]
fn scion_of_the_ur_dragon_stays_itself_when_the_search_finds_no_dragon() {
    let Some(db) = db() else { return };
    let mut s = scenario(2);
    let scion = s.add_real_card(P0, "Scion of the Ur-Dragon", Zone::Battlefield, db);
    let bears = s.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    s.with_mana_pool(P0, pool(&[(ManaType::Colorless, 2)]));
    let mut runner = s.build();
    let search = ability_index_with_description(&runner, scion, "Search your library");
    runner
        .activate(scion, search)
        .search_first_legal()
        .resolve();

    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "reach-guard: the {{2}} was paid, so the ability resolved"
    );
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Library,
        "nothing was found"
    );
    assert_eq!(
        runner.state().objects[&scion].name,
        "Scion of the Ur-Dragon",
        "no card was put into the graveyard, so it does not become a copy"
    );
    assert_eq!(power_toughness(&runner, scion), (4, 4), "still printed 4/4");
}

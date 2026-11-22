//! Runtime witnesses for the `confirmed-misparses.toml` rows released in batch 6: census rows whose cited clauses
//! the engine already handles, one witness group per clause shape. Every released card is the printed card
//! (printed cost, type line, P/T and verbatim Oracle text, loaded from the card database through `add_real_card`)
//! and is driven through the public `GameRunner` actions. Only textless library fillers are synthetic; every support
//! card, including those that carry an ability (Murder, the library cards), is a
//! printed card.
//!
//! Each group asserts the prompt the engine shows (actor and offered cards) before the resolved effect, and ends
//! with an empty stack and priority. Controls (a third legendary creature, a creature card that does not share the
//! shape) are there to fail if the clause were parsed wrongly.
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn db() -> &'static engine::database::card_db::CardDatabase {
    shared_card_db().expect("the committed integration card fixture is present")
}

/// Pool mana for every printed cost used below, so that a cast pays exactly its printed
/// cost and never taps a land the test needs untapped.
fn pool() -> Vec<ManaUnit> {
    let mut kinds = Vec::new();
    for kind in [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ] {
        kinds.extend([kind; 6]);
    }
    kinds.extend([ManaType::Colorless; 4]);
    kinds
        .into_iter()
        .enumerate()
        .map(|(i, kind)| ManaUnit::new(kind, ObjectId(9900 + i as u64), false, vec![]))
        .collect()
}

/// Two players, P0 to act in the precombat main phase with `pool()`.
fn table() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool());
    scenario
}

/// Library cards, top first, as printed cards.
fn stack_library(
    scenario: &mut GameScenario,
    player: PlayerId,
    names_top_first: &[&str],
) -> Vec<ObjectId> {
    names_top_first
        .iter()
        .map(|name| scenario.add_real_card(player, name, Zone::Library, db()))
        .collect()
}

fn library_of(r: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    r.state().players[player.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

fn zone_of(r: &GameRunner, id: ObjectId) -> Zone {
    r.state().objects[&id].zone
}

fn assert_settled(r: &GameRunner, what: &str) {
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "{what}: ends at Priority, got {:?}",
        r.state().waiting_for
    );
    assert!(r.state().stack.is_empty(), "{what}: the stack is empty");
}

/// Pass priority while an object is on the stack and the engine is offering priority; returns the
/// state the engine stopped in (a prompt, or priority with an empty stack).
fn resolve_stack(r: &mut GameRunner) -> WaitingFor {
    for _ in 0..40 {
        if !matches!(r.state().waiting_for, WaitingFor::Priority { .. })
            || r.state().stack.is_empty()
        {
            break;
        }
        r.act(GameAction::PassPriority).expect("pass priority");
    }
    r.state().waiting_for.clone()
}

fn cast(r: &mut GameRunner, spell: ObjectId) {
    let card_id = r.state().objects[&spell].card_id;
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast");
}

fn activate(r: &mut GameRunner, source: ObjectId, index: usize) {
    r.act(GameAction::ActivateAbility {
        source_id: source,
        ability_index: index,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("activate");
}

fn choose_object(r: &mut GameRunner, id: ObjectId) {
    r.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(id)),
    })
    .expect("choose target");
}

/// The legal targets of the slot the engine is asking about (cast-time or trigger-time prompt).
fn offered_targets(r: &GameRunner) -> Vec<ObjectId> {
    let (player, selection) = match &r.state().waiting_for {
        WaitingFor::TargetSelection {
            player, selection, ..
        }
        | WaitingFor::TriggerTargetSelection {
            player, selection, ..
        } => (*player, selection),
        other => panic!("expected a target prompt, got {other:?}"),
    };
    assert_eq!(player, P0, "P0 announces the targets");
    let mut ids: Vec<ObjectId> = selection
        .current_legal_targets
        .iter()
        .map(|t| match t {
            TargetRef::Object(o) => *o,
            TargetRef::Player(p) => panic!("an object prompt offered player {p:?}"),
        })
        .collect();
    ids.sort_by_key(|o| o.0);
    ids
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort_by_key(|o| o.0);
    ids
}

fn power_toughness(r: &GameRunner, id: ObjectId) -> (i32, i32) {
    let o = &r.state().objects[&id];
    (o.power.unwrap_or(0), o.toughness.unwrap_or(0))
}

fn attack_with(r: &mut GameRunner, attackers: &[ObjectId]) {
    r.advance_to_combat();
    assert!(
        matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. }),
        "the declare-attackers prompt, got {:?}",
        r.state().waiting_for
    );
    let attacks: Vec<_> = attackers
        .iter()
        .map(|id| (*id, engine::game::combat::AttackTarget::Player(P1)))
        .collect();
    r.declare_attackers(&attacks).expect("declare attackers");
    resolve_stack(r);
}

/// Advance (default answers) until `done` holds.
fn drive_until(r: &mut GameRunner, label: &str, done: impl Fn(&GameRunner) -> bool) {
    for _ in 0..600 {
        if done(r) {
            return;
        }
        let action = match r.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            ref o => panic!("{label}: unexpected prompt {o:?}"),
        };
        r.act(action).expect(label);
    }
    panic!("{label}: never reached");
}

// ---------------------------------------------------------------------------------------------
// "{T}: Untap two other target legendary creatures." (Ioreth of the Healing House)
// ---------------------------------------------------------------------------------------------

/// CR 115.1c + CR 115.3 + CR 602.2: the second ability declares exactly two other legendary
/// creatures; each slot offers only legendary creatures other than the source and other than the one
/// already chosen, both untap on resolution and a third legendary creature stays tapped.
#[test]
fn ioreth_untaps_exactly_two_other_legendary_creatures() {
    let mut scenario = table();
    let ioreth = scenario.add_real_card(P0, "Ioreth of the Healing House", Zone::Battlefield, db());
    let isamaru = scenario.add_real_card(P0, "Isamaru, Hound of Konda", Zone::Battlefield, db());
    let tyrox = scenario.add_real_card(P0, "Tyrox, Saurid Tyrant", Zone::Battlefield, db());
    // An opponent's legendary creature is a legal target too ("other target legendary creatures").
    let caelorna = scenario.add_real_card(P1, "Caelorna, Coral Tyrant", Zone::Battlefield, db());
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db());
    let mut runner = scenario.build();
    for id in [isamaru, tyrox, caelorna, bears] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }

    activate(&mut runner, ioreth, 1);
    assert_eq!(
        offered_targets(&runner),
        sorted(vec![isamaru, tyrox, caelorna]),
        "first slot: legendary creatures other than Ioreth (not the non-legendary bear)"
    );
    choose_object(&mut runner, isamaru);
    assert_eq!(
        offered_targets(&runner),
        sorted(vec![tyrox, caelorna]),
        "second slot: not the creature already chosen, not Ioreth, not the bear"
    );
    choose_object(&mut runner, tyrox);
    resolve_stack(&mut runner);
    assert_settled(&runner, "Ioreth");
    let tapped = |id| runner.state().objects[&id].tapped;
    assert!(tapped(ioreth), "the {{T}} cost was paid");
    assert!(
        !tapped(isamaru) && !tapped(tyrox),
        "both chosen creatures untap"
    );
    assert!(
        tapped(caelorna),
        "the unchosen legendary creature stays tapped"
    );
    assert!(tapped(bears), "the non-legendary creature stays tapped");
}

/// Ioreth's first ability: "{T}: Untap another target permanent." -- one permanent of any kind,
/// never Ioreth itself.
#[test]
fn ioreth_first_ability_untaps_one_other_permanent() {
    let mut scenario = table();
    let ioreth = scenario.add_real_card(P0, "Ioreth of the Healing House", Zone::Battlefield, db());
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db());
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db());
    let mut runner = scenario.build();
    for id in [forest, bears] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    activate(&mut runner, ioreth, 0);
    assert_eq!(
        offered_targets(&runner),
        sorted(vec![forest, bears]),
        "another permanent: not Ioreth"
    );
    choose_object(&mut runner, forest);
    resolve_stack(&mut runner);
    assert!(
        !runner.state().objects[&forest].tapped,
        "the chosen land untaps"
    );
    assert!(
        runner.state().objects[&bears].tapped,
        "only one permanent untaps"
    );
    assert_settled(&runner, "Ioreth first ability");
}

// ---------------------------------------------------------------------------------------------
// "Search your library for <a card>, reveal it, then shuffle and put that card on top." (Loyal Tutor)
// ---------------------------------------------------------------------------------------------

/// CR 701.24b: the found card is not included in the shuffle; it is put on top of the library. The
/// shuffle is random, so the witness runs over several seeds; a missing put-on-top would leave the
/// card on top only by chance (1 in 9 per seed).
#[test]
fn loyal_tutor_puts_the_found_planeswalker_on_top_after_the_shuffle() {
    let mut shuffled_in_some_seed = false;
    for seed in 1..=6u64 {
        let mut scenario = GameScenario::new_n_player(2, seed);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, pool());
        let spell = scenario.add_real_card(P0, "Loyal Tutor", Zone::Hand, db());
        let lib = stack_library(
            &mut scenario,
            P0,
            &[
                "Jace, the Mind Sculptor",
                "Forest",
                "Island",
                "Plains",
                "Swamp",
                "Mountain",
                "Grizzly Bears",
                "Llanowar Elves",
                "Chandra, Torch of Defiance",
            ],
        );
        let mut runner = scenario.build();
        let chandra = lib[8];
        cast(&mut runner, spell);
        let waiting = resolve_stack(&mut runner);
        let WaitingFor::SearchChoice {
            player,
            cards,
            count,
            reveal,
            ..
        } = waiting
        else {
            panic!("seed {seed}: the search opens a choice, got {waiting:?}");
        };
        assert_eq!(player, P0);
        assert_eq!(count, 1, "a planeswalker card");
        assert!(reveal, "reveal it");
        assert_eq!(
            sorted(cards),
            sorted(vec![lib[0], chandra]),
            "only the two planeswalker cards are findable"
        );
        runner
            .act(GameAction::SelectCards {
                cards: vec![chandra],
            })
            .expect("take Chandra");
        resolve_stack(&mut runner);
        let library = library_of(&runner, P0);
        assert_eq!(
            library.len(),
            9,
            "seed {seed}: the card is still in the library"
        );
        assert_eq!(library[0], chandra, "seed {seed}: the found card is on top");
        shuffled_in_some_seed |= library[1..] != lib[..8];
        assert_settled(&runner, "Loyal Tutor");
    }
    assert!(
        shuffled_in_some_seed,
        "the rest of the library is shuffled: its order changes in at least one seed"
    );
}

// ---------------------------------------------------------------------------------------------
// "Exalted ... Other creatures you control have exalted." (Sublime Archangel)
// ---------------------------------------------------------------------------------------------

/// CR 702.83a + CR 702.83b: a creature attacking alone gets +1/+1 until end of turn for each exalted
/// trigger; the Archangel's own exalted and the one it grants to the bear are two instances (the
/// Archangel's reminder text), so the lone bear attacks as a 4/4 until end of turn.
#[test]
fn sublime_archangel_exalted_pumps_a_lone_attacker_once_per_instance() {
    let mut scenario = table();
    let archangel = scenario.add_real_card(P0, "Sublime Archangel", Zone::Battlefield, db());
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db());
    scenario.with_library_top(P0, &["P0 Draw 1", "P0 Draw 2", "P0 Draw 3"]);
    scenario.with_library_top(P1, &["P1 Draw 1", "P1 Draw 2", "P1 Draw 3"]);
    let mut runner = scenario.build();
    assert_eq!(power_toughness(&runner, bears), (2, 2));
    attack_with(&mut runner, &[bears]);
    assert_eq!(
        power_toughness(&runner, bears),
        (4, 4),
        "two instances of exalted: the Archangel's own and the one it grants to the bear"
    );
    assert_eq!(
        power_toughness(&runner, archangel),
        (4, 3),
        "the Archangel did not attack"
    );
    let outcome = runner.combat_damage();
    outcome.assert_life_delta(P1, -4);
    // The +1/+1 lasts until end of turn (CR 702.83a): still on in the end step, gone after cleanup.
    drive_until(&mut runner, "end step", |r| {
        r.state().phase == Phase::End
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
    });
    assert_eq!(
        power_toughness(&runner, bears),
        (4, 4),
        "still pumped in the end step, after combat"
    );
    drive_until(&mut runner, "next turn", |r| {
        r.state().turn_number >= 2 && r.state().phase == Phase::Upkeep
    });
    assert_eq!(
        power_toughness(&runner, bears),
        (2, 2),
        "the pump ended with the turn"
    );
    assert_settled(&runner, "Sublime Archangel (lone attacker)");
}

/// CR 702.83b: "attacks alone" -- with two attackers neither exalted instance triggers.
#[test]
fn sublime_archangel_exalted_does_not_trigger_when_two_creatures_attack() {
    let mut scenario = table();
    let archangel = scenario.add_real_card(P0, "Sublime Archangel", Zone::Battlefield, db());
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db());
    let mut runner = scenario.build();
    attack_with(&mut runner, &[bears, archangel]);
    assert_eq!(power_toughness(&runner, bears), (2, 2));
    assert_eq!(power_toughness(&runner, archangel), (4, 3));
    let outcome = runner.combat_damage();
    outcome.assert_life_delta(P1, -6);
    drive_until(&mut runner, "postcombat main", |r| {
        r.state().phase == Phase::PostCombatMain
            && matches!(r.state().waiting_for, WaitingFor::Priority { .. })
    });
    assert_settled(&runner, "Sublime Archangel (two attackers)");
}

// ---------------------------------------------------------------------------------------------
// "... earthbend X, where X is that creature's power." (Beifong's Bounty Hunters)
// ---------------------------------------------------------------------------------------------

fn settle_earthbend(runner: &mut GameRunner, offered: Vec<ObjectId>, land: ObjectId) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 20, "never settled");
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { player: P0, .. } => {
                assert_eq!(offered_targets(runner), offered, "target land you control");
                choose_object(runner, land);
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("earthbend: unexpected prompt {other:?}"),
        }
    }
}

#[test]
fn beifongs_bounty_hunters_earthbend_x_is_the_dead_creatures_power() {
    use engine::types::counter::CounterType;
    let mut scenario = table();
    let _hunters = scenario.add_real_card(P0, "Beifong's Bounty Hunters", Zone::Battlefield, db());
    // Tyrox is a 4/1: X must be its power (4), not its toughness (1).
    let giant = scenario.add_real_card(P0, "Tyrox, Saurid Tyrant", Zone::Battlefield, db());
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db());
    let _their_island = scenario.add_real_card(P1, "Island", Zone::Battlefield, db());
    let murder = scenario.add_real_card(P0, "Murder", Zone::Hand, db());
    let mut runner = scenario.build();
    cast(&mut runner, murder);
    choose_object(&mut runner, giant);
    settle_earthbend(&mut runner, vec![forest], forest);
    assert_eq!(zone_of(&runner, giant), Zone::Graveyard);
    let land = &runner.state().objects[&forest];
    assert!(
        land.card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature),
        "it is a creature"
    );
    assert!(
        land.card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Land),
        "and still a land"
    );
    assert!(
        land.keywords
            .contains(&engine::types::keywords::Keyword::Haste),
        "earthbend gives the land haste"
    );
    assert_eq!(
        land.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(4),
        "X = Tyrox's power 4"
    );
    assert_eq!(
        (land.power, land.toughness),
        (Some(4), Some(4)),
        "0/0 plus four +1/+1 counters"
    );
    assert_settled(&runner, "earthbend");
}

/// Beifong's Bounty Hunters: "When it dies or is exiled, return it to the battlefield tapped" -- the
/// animated land that earthbend made comes back as a tapped land when it dies.
#[test]
fn beifongs_bounty_hunters_earthbent_land_returns_tapped_when_it_dies() {
    let mut scenario = table();
    let _hunters = scenario.add_real_card(P0, "Beifong's Bounty Hunters", Zone::Battlefield, db());
    let giant = scenario.add_real_card(P0, "Hill Giant", Zone::Battlefield, db());
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db());
    let _their_island = scenario.add_real_card(P1, "Island", Zone::Battlefield, db());
    let mountain = scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db());
    let murder_one = scenario.add_real_card(P0, "Murder", Zone::Hand, db());
    let murder_two = scenario.add_real_card(P0, "Murder", Zone::Hand, db());
    let mut runner = scenario.build();
    // First Murder kills the Hill Giant; its death earthbends the Forest.
    cast(&mut runner, murder_one);
    choose_object(&mut runner, giant);
    settle_earthbend(&mut runner, sorted(vec![forest, mountain]), forest);
    assert!(
        runner.state().objects[&forest]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature),
        "the Forest is a creature"
    );
    // X is the dead creature's power (Hill Giant 3), not Beifong's own power (4/4).
    assert_eq!(
        runner.state().objects[&forest]
            .counters
            .get(&engine::types::counter::CounterType::Plus1Plus1)
            .copied(),
        Some(3),
        "three +1/+1 counters for the 3/3 Hill Giant"
    );
    // Second Murder kills the animated Forest; it dies, so it is returned to the battlefield tapped.
    cast(&mut runner, murder_two);
    choose_object(&mut runner, forest);
    let waiting = resolve_stack(&mut runner);
    assert!(
        matches!(waiting, WaitingFor::Priority { .. }),
        "got {waiting:?}"
    );
    // The dead creature is a land, so Beifong's "nonland creature" trigger does not fire. If it did, the
    // Mountain (the only land left that P0 controls) would be auto-targeted and earthbent, so it must still
    // be an ordinary land without counters. The delayed return puts the Forest back tapped.
    let mountain_object = &runner.state().objects[&mountain];
    assert!(
        !mountain_object
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature)
            && mountain_object.counters.is_empty(),
        "the Mountain was not earthbent"
    );
    let forests: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.name == "Forest" && o.zone == Zone::Battlefield)
        .collect();
    assert_eq!(forests.len(), 1, "one Forest is back on the battlefield");
    assert!(forests[0].tapped, "it returned tapped");
    assert!(
        !forests[0]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature),
        "as an ordinary land again"
    );
    assert_settled(&runner, "earthbend return");
}

/// Control: "a nonland creature you control dies" -- an opponent's creature dying does not earthbend.
#[test]
fn beifongs_bounty_hunters_ignores_an_opponents_creature_dying() {
    let mut scenario = table();
    let _hunters = scenario.add_real_card(P0, "Beifong's Bounty Hunters", Zone::Battlefield, db());
    let theirs = scenario.add_real_card(P1, "Hill Giant", Zone::Battlefield, db());
    let forest = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db());
    let murder = scenario.add_real_card(P0, "Murder", Zone::Hand, db());
    let mut runner = scenario.build();
    cast(&mut runner, murder);
    choose_object(&mut runner, theirs);
    let waiting = resolve_stack(&mut runner);
    assert!(
        matches!(waiting, WaitingFor::Priority { .. }),
        "no trigger prompt, got {waiting:?}"
    );
    assert_eq!(zone_of(&runner, theirs), Zone::Graveyard);
    assert!(
        !runner.state().objects[&forest]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature),
        "the Forest was not earthbent"
    );
    assert_settled(&runner, "Beifong control");
}

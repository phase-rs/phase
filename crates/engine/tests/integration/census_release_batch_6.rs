//! Runtime witnesses for the `confirmed-misparses.toml` rows released in batch 6: seven census rows whose cited clauses
//! the engine already handles, one witness group per clause shape. Every released card is the printed card
//! (printed cost, type line, P/T and verbatim Oracle text, loaded from the card database through `add_real_card`)
//! and is driven through the public `GameRunner` actions. Only textless library fillers are synthetic; every support
//! card, including those that carry an ability (Murder, Heart of Kiran, Lightning Bolt, Shock, the library cards), is a
//! printed card.
//!
//! Each group asserts the prompt the engine shows (actor and offered cards) before the resolved effect, and ends
//! with an empty stack and priority. Controls (a third legendary creature, a creature card that does not share the
//! shape, a declined optional cast) are there to fail if the clause were parsed wrongly.
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
        .filter_map(|t| match t {
            TargetRef::Object(o) => Some(*o),
            TargetRef::Player(_) => None,
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
    let caelorna = scenario.add_real_card(P0, "Caelorna, Coral Tyrant", Zone::Battlefield, db());
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
// "Reveal the top six cards ... put an Assassin, Pirate, or Vehicle card from among them into your hand and the rest on
// the bottom of your library in a random order" (Adéwalé, Breaker of Chains)
// ---------------------------------------------------------------------------------------------

#[test]
fn adewale_takes_one_assassin_pirate_or_vehicle_and_bottoms_the_rest() {
    let mut scenario = table();
    let adewale = scenario.add_real_card(P0, "Adéwalé, Breaker of Chains", Zone::Hand, db());
    let lib = stack_library(
        &mut scenario,
        P0,
        &[
            "Smuggler's Copter",
            "Cloud Pirates",
            "Guul Draz Assassin",
            "Grizzly Bears",
            "Forest",
            "Island",
            "Plains",
            "Swamp",
        ],
    );
    let mut runner = scenario.build();
    let [copter, pirates, assassin, bears, forest, island, plains, swamp] = lib[..] else {
        unreachable!()
    };
    cast(&mut runner, adewale);
    let waiting = resolve_stack(&mut runner);
    let WaitingFor::DigChoice {
        player,
        cards,
        selectable_cards,
        keep_count,
        up_to,
        ..
    } = waiting
    else {
        panic!("the reveal opens a choice, got {waiting:?}");
    };
    assert_eq!(player, P0);
    assert_eq!(
        sorted(cards),
        sorted(vec![copter, pirates, assassin, bears, forest, island])
    );
    assert_eq!(
        sorted(selectable_cards),
        sorted(vec![copter, pirates, assassin]),
        "an Assassin, a Pirate or a Vehicle"
    );
    // One card is taken. Like every Dig prompt the engine lets the player select none (`up_to`), which also
    // covers a reveal with no qualifying card; this change does not alter that shared convention.
    assert_eq!((keep_count, up_to), (1, true));
    runner
        .act(GameAction::SelectCards {
            cards: vec![pirates],
        })
        .expect("take Cloud Pirates");
    resolve_stack(&mut runner);
    assert_eq!(zone_of(&runner, pirates), Zone::Hand);
    let library = library_of(&runner, P0);
    assert_eq!(library[..2], [plains, swamp]);
    assert_eq!(
        sorted(library[2..].to_vec()),
        sorted(vec![copter, assassin, bears, forest, island]),
        "the other five revealed cards are on the bottom"
    );
    assert_settled(&runner, "Adéwalé");
}

/// Adéwalé's second ability: "Whenever a Vehicle you control deals combat damage to a player, you may
/// return this card from your graveyard to your hand." Accepted it returns; declined it stays.
#[test]
fn adewale_returns_from_the_graveyard_when_a_vehicle_deals_combat_damage() {
    for accept in [true, false] {
        let mut scenario = table();
        let adewale =
            scenario.add_real_card(P0, "Adéwalé, Breaker of Chains", Zone::Graveyard, db());
        let heart = scenario.add_real_card(P0, "Heart of Kiran", Zone::Battlefield, db());
        let crew = scenario.add_real_card(P0, "Hill Giant", Zone::Battlefield, db());
        scenario.with_library_top(P0, &["P0 Draw 1", "P0 Draw 2"]);
        scenario.with_library_top(P1, &["P1 Draw 1", "P1 Draw 2"]);
        let mut runner = scenario.build();
        runner
            .act(GameAction::CrewVehicle {
                vehicle_id: heart,
                creature_ids: vec![],
            })
            .expect("enter crew selection");
        runner
            .act(GameAction::CrewVehicle {
                vehicle_id: heart,
                creature_ids: vec![crew],
            })
            .expect("crew Heart of Kiran with the Hill Giant (crew 3)");
        resolve_stack(&mut runner);
        attack_with(&mut runner, &[heart]);
        runner.combat_damage();
        assert_eq!(
            runner.state().players[P1.0 as usize].life,
            16,
            "the Vehicle dealt 4"
        );
        let mut asked = false;
        let mut guard = 0;
        loop {
            guard += 1;
            assert!(guard < 20, "never settled");
            match runner.state().waiting_for.clone() {
                WaitingFor::OptionalEffectChoice { player, .. } => {
                    assert_eq!(player, P0, "its controller decides");
                    asked = true;
                    runner
                        .act(GameAction::DecideOptionalEffect { accept })
                        .expect("answer the may");
                }
                WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                    runner.act(GameAction::PassPriority).expect("pass");
                }
                WaitingFor::Priority { .. } => break,
                other => panic!("Adéwalé return: unexpected prompt {other:?}"),
            }
        }
        assert!(asked, "\"you may\" is offered");
        assert_eq!(
            zone_of(&runner, adewale),
            if accept { Zone::Hand } else { Zone::Graveyard },
            "accept = {accept}"
        );
        assert_settled(&runner, "Adéwalé return");
    }
}

/// Control: only a Vehicle's combat damage brings Adéwalé back; an ordinary creature dealing combat
/// damage to the player does not.
#[test]
fn adewale_stays_in_the_graveyard_when_an_ordinary_creature_deals_combat_damage() {
    let mut scenario = table();
    let adewale = scenario.add_real_card(P0, "Adéwalé, Breaker of Chains", Zone::Graveyard, db());
    let giant = scenario.add_real_card(P0, "Hill Giant", Zone::Battlefield, db());
    scenario.with_library_top(P0, &["P0 Draw 1", "P0 Draw 2"]);
    scenario.with_library_top(P1, &["P1 Draw 1", "P1 Draw 2"]);
    let mut runner = scenario.build();
    attack_with(&mut runner, &[giant]);
    runner.combat_damage();
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        17,
        "the Hill Giant dealt 3"
    );
    let waiting = resolve_stack(&mut runner);
    assert!(
        matches!(waiting, WaitingFor::Priority { .. }),
        "no return offer, got {waiting:?}"
    );
    assert_eq!(zone_of(&runner, adewale), Zone::Graveyard);
    assert_settled(&runner, "Adéwalé control");
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
// "Copy that card three times. You may cast the copies without paying their mana costs." (Mnemonic Deluge)
// ---------------------------------------------------------------------------------------------

#[test]
fn mnemonic_deluge_copies_the_exiled_card_three_times_and_casts_the_copies() {
    let mut scenario = table();
    let deluge = scenario.add_real_card(P0, "Mnemonic Deluge", Zone::Hand, db());
    let bolt = scenario.add_real_card(P1, "Lightning Bolt", Zone::Graveyard, db());
    let shock = scenario.add_real_card(P0, "Shock", Zone::Graveyard, db());
    let axe = scenario.add_real_card(P1, "Lava Axe", Zone::Graveyard, db());
    let giant = scenario.add_real_card(P0, "Hill Giant", Zone::Graveyard, db());
    let mut runner = scenario.build();
    cast(&mut runner, deluge);
    assert_eq!(
        offered_targets(&runner),
        sorted(vec![bolt, shock, axe]),
        "instant and sorcery cards in any graveyard, not the creature card {giant:?}"
    );
    choose_object(&mut runner, bolt);
    let pool_after_cast = runner.state().players[P0.0 as usize].mana_pool.total();
    assert_eq!(
        pool_after_cast,
        pool().len() - 9,
        "Mnemonic Deluge's printed {{6}}{{U}}{{U}}{{U}} was paid"
    );
    let mut offers = 0;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::CopyRetarget {
                player,
                target_slots,
                ..
            } => {
                assert_eq!(player, P0, "the caster announces the copy's target");
                assert!(
                    target_slots[0]
                        .legal_alternatives
                        .contains(&TargetRef::Player(P1)),
                    "any target: the opponent is offered"
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("the copy targets the opponent");
            }
            WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
                assert_eq!(player, P0, "its controller may cast the copy");
                assert_eq!(
                    cards,
                    vec![bolt],
                    "the copied card is the exiled Lightning Bolt"
                );
                offers += 1;
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("cast a copy of it");
            }
            other => panic!("Mnemonic Deluge: unexpected prompt {other:?}"),
        }
    }
    assert_eq!(offers, 3, "three copies are offered");
    let bolts = runner
        .state()
        .objects
        .values()
        .filter(|o| o.name == "Lightning Bolt")
        .count();
    assert_eq!(
        bolts, 1,
        "the cast copies ceased to exist: only the exiled original remains"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        11,
        "three Lightning Bolt copies hit for 3"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        pool_after_cast,
        "the copies were cast without paying their mana costs"
    );
    assert_eq!(
        zone_of(&runner, bolt),
        Zone::Exile,
        "the original was exiled"
    );
    assert_eq!(
        zone_of(&runner, shock),
        Zone::Graveyard,
        "the other card was not chosen"
    );
    assert_eq!(
        zone_of(&runner, deluge),
        Zone::Exile,
        "Mnemonic Deluge exiles itself"
    );
    assert_settled(&runner, "Mnemonic Deluge");
}

/// Control: "You may cast the copies" -- declining every copy casts none (no damage), while the
/// first sentence still exiled the card and the last still exiles Mnemonic Deluge.
#[test]
fn mnemonic_deluge_casting_none_of_the_copies_deals_no_damage() {
    let mut scenario = table();
    let deluge = scenario.add_real_card(P0, "Mnemonic Deluge", Zone::Hand, db());
    let bolt = scenario.add_real_card(P1, "Lightning Bolt", Zone::Graveyard, db());
    let shock = scenario.add_real_card(P0, "Shock", Zone::Graveyard, db());
    let mut runner = scenario.build();
    cast(&mut runner, deluge);
    choose_object(&mut runner, bolt);
    let mut offers = 0;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::ChooseFromZoneChoice { player, .. } => {
                assert_eq!(player, P0);
                offers += 1;
                runner
                    .act(GameAction::SelectCards { cards: vec![] })
                    .expect("cast no copy");
            }
            other => panic!("Mnemonic Deluge (declined): unexpected prompt {other:?}"),
        }
    }
    assert_eq!(offers, 3, "the three copies were offered and declined");
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        20,
        "no copy was cast"
    );
    assert_eq!(zone_of(&runner, bolt), Zone::Exile);
    assert_eq!(zone_of(&runner, shock), Zone::Graveyard);
    assert_eq!(zone_of(&runner, deluge), Zone::Exile);
    assert_settled(&runner, "Mnemonic Deluge declined");
}

// ---------------------------------------------------------------------------------------------
// "Mill seven cards. Then put a creature card from among them onto the battlefield." (Bind to Life, the prepare spell of
// Vastlands Scavenger)
// ---------------------------------------------------------------------------------------------

#[test]
fn bind_to_life_puts_a_creature_card_milled_this_way_onto_the_battlefield() {
    let mut scenario = table();
    let scavenger = scenario.add_real_card(P0, "Vastlands Scavenger", Zone::Hand, db());
    let old_giant = scenario.add_real_card(P0, "Hill Giant", Zone::Graveyard, db());
    let lib = stack_library(
        &mut scenario,
        P0,
        &[
            "Grizzly Bears",
            "Forest",
            "Llanowar Elves",
            "Island",
            "Plains",
            "Swamp",
            "Mountain",
            "Serra Angel",
        ],
    );
    let mut runner = scenario.build();
    let [bears, forest, elves, island, plains, swamp, mountain, angel] = lib[..] else {
        unreachable!()
    };
    cast(&mut runner, scavenger);
    resolve_stack(&mut runner);
    assert_eq!(zone_of(&runner, scavenger), Zone::Battlefield);
    runner
        .act(GameAction::CastPreparedCopy { source: scavenger })
        .expect("cast the prepared copy of Bind to Life");
    let mut prompted = false;
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 20, "never settled");
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            WaitingFor::Priority { .. } => break,
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                up_to,
                zone,
                destination,
                ..
            } => {
                prompted = true;
                assert_eq!(
                    (zone, destination),
                    (Zone::Graveyard, Some(Zone::Battlefield)),
                    "from the graveyard onto the battlefield"
                );
                assert_eq!(player, P0, "the caster chooses the creature");
                // One card; the engine lets the player take none (`up_to`), as for every "from among them" choice.
                assert_eq!((count, up_to), (1, true));
                assert_eq!(
                    sorted(cards),
                    sorted(vec![bears, elves]),
                    "creature cards among the seven milled, not the Hill Giant already in the graveyard"
                );
                runner
                    .act(GameAction::SelectCards { cards: vec![elves] })
                    .expect("put Llanowar Elves onto the battlefield");
            }
            other => panic!("Bind to Life: unexpected prompt {other:?}"),
        }
    }
    assert!(prompted, "the creature choice was offered");
    assert_eq!(zone_of(&runner, elves), Zone::Battlefield);
    for milled in [bears, forest, island, plains, swamp, mountain] {
        assert_eq!(
            zone_of(&runner, milled),
            Zone::Graveyard,
            "seven cards were milled"
        );
    }
    assert_eq!(zone_of(&runner, old_giant), Zone::Graveyard);
    assert_eq!(
        library_of(&runner, P0),
        vec![angel],
        "only the eighth card is left"
    );
    assert_settled(&runner, "Bind to Life");
}

/// Control: "a creature card from among them" -- with no creature among the seven milled cards,
/// nothing enters, and a creature in the eighth position (not milled) stays in the library.
#[test]
fn bind_to_life_with_no_creature_among_the_milled_cards_puts_nothing_onto_the_battlefield() {
    let mut scenario = table();
    let scavenger = scenario.add_real_card(P0, "Vastlands Scavenger", Zone::Hand, db());
    let lib = stack_library(
        &mut scenario,
        P0,
        &[
            "Forest",
            "Island",
            "Plains",
            "Swamp",
            "Mountain",
            "Forest",
            "Island",
            "Llanowar Elves",
        ],
    );
    let mut runner = scenario.build();
    cast(&mut runner, scavenger);
    resolve_stack(&mut runner);
    runner
        .act(GameAction::CastPreparedCopy { source: scavenger })
        .expect("cast the prepared copy of Bind to Life");
    resolve_stack(&mut runner);
    assert_settled(&runner, "Bind to Life (no creature milled)");
    for milled in &lib[..7] {
        assert_eq!(
            zone_of(&runner, *milled),
            Zone::Graveyard,
            "seven cards were milled"
        );
    }
    assert_eq!(
        zone_of(&runner, lib[7]),
        Zone::Library,
        "the eighth card was not milled, so not offered"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|o| o.name == "Llanowar Elves" && o.zone == Zone::Battlefield)
            .count(),
        0
    );
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
    // The dead creature is a land, so Beifong's "nonland creature" trigger does not fire (it would have
    // offered the Mountain as a target and stopped above); the delayed return puts the Forest back tapped.
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

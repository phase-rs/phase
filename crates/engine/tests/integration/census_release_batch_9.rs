//! Runtime witnesses for the 13 `wrong_object` rows released from `confirmed-misparses.toml`
//! in batch 9. Every card, including the controls and library fillers, is the printed card
//! loaded through `add_real_card`. The scenarios supply the starting board, libraries, and
//! synthetic mana pools, then drive the printed cards through public `GameRunner` actions.
//!
//! One group per clause shape: search and put the found card on top, and Aura-relative
//! targeting restrictions. The search controls cover ineligible cards, finding nothing,
//! declining the search, choosing another mode, and a mandatory search without a reveal.
//! The search-to-top group asserts the visible CR 701.24b outcome: the rest of the library
//! is randomized, the found card ends up on top, and one shuffle occurs. The engine currently
//! shuffles the found card with the rest first.
//! The Aura controls check an unenchanted creature, different host controllers, a change
//! of Aura controller, and targeting before attachment and after removal.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn card(s: &mut GameScenario, player: PlayerId, name: &str, zone: Zone) -> ObjectId {
    s.add_real_card(
        player,
        name,
        zone,
        shared_card_db().expect("printed fixture"),
    )
}

// CR 106.4: Seed mana pools to pay printed costs.
fn fund(s: &mut GameScenario, player: PlayerId) {
    let mana = [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
        ManaType::Colorless,
    ]
    .into_iter()
    .flat_map(|kind| (0..12).map(move |_| ManaUnit::new(kind, ObjectId(0), false, vec![])))
    .collect();
    s.with_mana_pool(player, mana);
}

fn table() -> GameScenario {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    fund(&mut s, P0);
    fund(&mut s, P1);
    s
}

fn library(r: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    r.state().players[player.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

fn stock(s: &mut GameScenario, player: PlayerId, names: &[&str]) -> Vec<ObjectId> {
    names
        .iter()
        .map(|name| card(s, player, name, Zone::Library))
        .collect()
}

fn act(r: &mut GameRunner, action: GameAction) -> Vec<GameEvent> {
    r.act(action).expect("public game action").events
}

// CR 601.2c: Begin casting and let the engine prompt for any required targets.
fn announce(r: &mut GameRunner, spell: ObjectId) {
    act(
        r,
        GameAction::CastSpell {
            object_id: spell,
            card_id: r.state().objects[&spell].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        },
    );
}

// CR 117.4 + CR 805.5b: Pass in succession to resolve stack objects until a choice or an empty stack.
fn pass_stack(r: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..40 {
        if r.state().stack.is_empty()
            || !matches!(r.state().waiting_for, WaitingFor::Priority { .. })
        {
            return events;
        }
        events.extend(act(r, GameAction::PassPriority));
    }
    panic!("stack did not settle");
}

// CR 117.3b + CR 805.5b: Resolution returns to priority after the stack empties.
fn settled(r: &GameRunner) {
    assert!(r.state().stack.is_empty(), "{:?}", r.state().stack);
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        r.state().waiting_for
    );
}

fn shuffles(events: &[GameEvent], player: PlayerId) -> usize {
    events.iter().filter(|event| matches!(event,
        GameEvent::PlayerPerformedAction { player_id, action: PlayerActionKind::ShuffledLibrary, .. }
            if *player_id == player
    )).count()
}

fn reveals(events: &[GameEvent], card: ObjectId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event,
                GameEvent::CardsRevealed { card_ids, .. } if card_ids.contains(&card)
            )
        })
        .count()
}

// CR 700.2: Choose the mode of a triggered ability through its mode prompt.
fn select_trigger_mode(r: &mut GameRunner, mode: usize) {
    assert!(
        matches!(
            r.state().waiting_for,
            WaitingFor::AbilityModeChoice { player: P0, .. }
        ),
        "{:?}",
        r.state().waiting_for
    );
    act(
        r,
        GameAction::SelectModes {
            indices: vec![mode],
        },
    );
}

// CR 701.23b + CR 701.24a: A printed search-and-shuffle gives the reference order for a failed find
// from the same seed and starting library.
fn empty_search_shuffle(names: &[&str]) -> Vec<usize> {
    let mut s = table();
    let wilds = card(&mut s, P0, "Evolving Wilds", Zone::Battlefield);
    let cards = stock(&mut s, P0, names);
    let mut r = s.build();
    r.activate(wilds, 0).resolve();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::SearchChoice { player: P0, .. }
    ));
    let mut events = act(&mut r, GameAction::SelectCards { cards: vec![] });
    events.extend(pass_stack(&mut r));
    assert_eq!(shuffles(&events, P0), 1);
    settled(&r);
    library_positions(&library(&r, P0), &cards)
}

fn library_positions(order: &[ObjectId], initial: &[ObjectId]) -> Vec<usize> {
    order
        .iter()
        .map(|id| {
            initial
                .iter()
                .position(|before| before == id)
                .expect("same cards")
        })
        .collect()
}

// CR 601.2i: A cast trigger resolves above the creature spell that triggered it.
fn assert_spell_on_stack(r: &GameRunner, spell: ObjectId, name: &str) {
    assert!(
        r.state().stack.iter().any(|entry| entry.id == spell),
        "{name}: expected the creature spell to still be on the stack"
    );
}

// CR 701.20a + CR 701.23a + CR 701.23b + CR 701.24a + CR 701.24b: Only matching cards are offered.
// Check both an empty choice and a find, which is revealed before the shuffle and finishes on top.
fn search_to_top(
    name: &str,
    mode: Option<usize>,
    names: &[&str],
    eligible: &[usize],
    optional: bool,
    cast_trigger: bool,
) {
    for pick in [None, Some(eligible[0])] {
        let mut s = table();
        let spell = card(&mut s, P0, name, Zone::Hand);
        let cards = stock(&mut s, P0, names);
        let mut r = s.build();
        announce(&mut r, spell);
        let mut events = pass_stack(&mut r);
        if let Some(mode) = mode {
            select_trigger_mode(&mut r, mode);
            events.extend(pass_stack(&mut r));
        }
        if optional {
            assert!(
                matches!(
                    r.state().waiting_for,
                    WaitingFor::OptionalEffectChoice { player: P0, .. }
                ),
                "{name}: {:?}",
                r.state().waiting_for
            );
            if cast_trigger {
                assert_spell_on_stack(&r, spell, name);
            }
            events.extend(act(
                &mut r,
                GameAction::DecideOptionalEffect { accept: true },
            ));
        }
        let WaitingFor::SearchChoice {
            player,
            cards: offered,
            count,
            allows_partial_find,
            ..
        } = &r.state().waiting_for
        else {
            panic!(
                "{name}: expected search choice, got {:?}",
                r.state().waiting_for
            );
        };
        if cast_trigger {
            assert_spell_on_stack(&r, spell, name);
        }
        assert_eq!(*player, P0, "{name}");
        assert_eq!(*count, 1, "{name}");
        assert!(*allows_partial_find, "{name}");
        let mut offered = offered.clone();
        offered.sort();
        let mut expected: Vec<_> = eligible.iter().map(|&index| cards[index]).collect();
        expected.sort();
        assert_eq!(offered, expected, "{name}");
        let found = pick.map(|index| cards[index]);
        events.extend(act(
            &mut r,
            GameAction::SelectCards {
                cards: found.into_iter().collect(),
            },
        ));
        if cast_trigger {
            assert_spell_on_stack(&r, spell, name);
        } else {
            events.extend(pass_stack(&mut r));
            settled(&r);
        }
        assert_eq!(shuffles(&events, P0), 1, "{name}, pick {pick:?}");
        assert_eq!(shuffles(&events, P1), 0, "{name}");
        for &id in &cards {
            assert_eq!(
                reveals(&events, id),
                usize::from(found == Some(id)),
                "{name}: {id:?}"
            );
        }
        let mut after = library(&r, P0);
        let placed = events.iter().position(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::PutAtLibraryPosition,
                    ..
                }
            )
        });
        if let Some(found) = found {
            let reveal = events
                .iter()
                .position(|event| reveals(std::slice::from_ref(event), found) > 0)
                .expect("found card revealed");
            let shuffle = events
                .iter()
                .position(|event| shuffles(std::slice::from_ref(event), P0) > 0)
                .expect("library shuffled");
            assert!(
                reveal < shuffle && shuffle < placed.expect("found card repositioned"),
                "{name}"
            );
            assert_eq!(after.first(), Some(&found), "{name}");
        } else {
            assert_eq!(
                library_positions(&after, &cards),
                empty_search_shuffle(names),
                "{name}: finding nothing leaves only the shuffle's visible order"
            );
        }
        after.sort();
        let mut before = cards;
        before.sort();
        assert_eq!(after, before, "{name}");
        if cast_trigger {
            pass_stack(&mut r);
            settled(&r);
            assert_eq!(r.state().objects[&spell].zone, Zone::Battlefield);
        }
    }
}

#[test]
fn conduit_of_ruin_offers_only_large_colorless_creatures_and_may_decline() {
    // CR 701.20a + CR 701.23a + CR 701.23b + CR 701.24a + CR 701.24b: Only large colorless creatures
    // qualify. A find is revealed and put on top after one shuffle, while declining leaves the library alone.
    let names = [
        "Forest",
        "Myr Battlesphere",
        "Darksteel Colossus",
        "Darksteel Forge",
        "Colossal Dreadmaw",
        "Steel Hellkite",
        "Pelakka Wurm",
    ];
    search_to_top("Conduit of Ruin", None, &names, &[1, 2], true, true);
    let mut s = table();
    let spell = card(&mut s, P0, "Conduit of Ruin", Zone::Hand);
    let cards = stock(&mut s, P0, &names);
    let mut r = s.build();
    r.cast(spell).commit().stop_at_optional().resolve();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));
    assert_spell_on_stack(&r, spell, "Conduit of Ruin");
    let events = act(&mut r, GameAction::DecideOptionalEffect { accept: false });
    assert_spell_on_stack(&r, spell, "Conduit of Ruin");
    assert_eq!(shuffles(&events, P0), 0);
    assert_eq!(library(&r, P0), cards);
    pass_stack(&mut r);
    settled(&r);
    assert_eq!(r.state().objects[&spell].zone, Zone::Battlefield);
}

#[test]
fn ainok_guide_search_mode_and_counter_mode() {
    // CR 700.2 + CR 701.20a + CR 701.23a + CR 701.23b + CR 701.24a + CR 701.24b: The search mode
    // reveals and tops a found basic land after one shuffle. The counter mode leaves the library alone.
    use engine::types::counter::CounterType;

    search_to_top(
        "Ainok Guide",
        Some(1),
        &["Evolving Wilds", "Forest", "Island"],
        &[1, 2],
        false,
        false,
    );
    let mut s = table();
    let spell = card(&mut s, P0, "Ainok Guide", Zone::Hand);
    let cards = stock(&mut s, P0, &["Forest", "Swamp"]);
    let mut r = s.build();
    announce(&mut r, spell);
    pass_stack(&mut r);
    select_trigger_mode(&mut r, 0);
    pass_stack(&mut r);
    assert_eq!(
        r.state().objects[&spell]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(1)
    );
    assert_eq!(library(&r, P0), cards);
    settled(&r);
}

#[test]
fn other_filtered_searches_reveal_and_put_the_found_card_on_top() {
    // CR 701.20a + CR 701.23a + CR 701.23b + CR 701.24a + CR 701.24b: Each filter permits an empty
    // find and reveals any card found before one shuffle and top placement.
    for (name, mode, cards, eligible, optional) in [
        (
            "Brutalizer Exarch",
            Some(0),
            ["Forest", "Grizzly Bears", "Sol Ring"],
            &[1][..],
            false,
        ),
        (
            "Compass Gnome",
            None,
            ["Evolving Wilds", "Hidden Nursery", "Forest"],
            &[1, 2][..],
            true,
        ),
        (
            "Flamekin Harbinger",
            None,
            ["Forest", "Mulldrifter", "Grizzly Bears"],
            &[1][..],
            true,
        ),
        (
            "Forerunner of the Coalition",
            None,
            ["Forest", "Siren Stormtamer", "Grizzly Bears"],
            &[1][..],
            true,
        ),
        (
            "Forerunner of the Empire",
            None,
            ["Forest", "Colossal Dreadmaw", "Grizzly Bears"],
            &[1][..],
            true,
        ),
        (
            "Forerunner of the Legion",
            None,
            ["Forest", "Vampire Nighthawk", "Grizzly Bears"],
            &[1][..],
            true,
        ),
        (
            "Giant Ladybug",
            None,
            ["Grizzly Bears", "Swamp", "Evolving Wilds"],
            &[1][..],
            true,
        ),
        (
            "Loam Larva",
            None,
            ["Evolving Wilds", "Mountain", "Grizzly Bears"],
            &[1][..],
            true,
        ),
    ] {
        search_to_top(name, mode, &cards, eligible, optional, false);
    }
}

#[test]
fn cruel_tutor_requires_a_find_and_does_not_reveal() {
    // CR 701.23d + CR 701.23e + CR 701.24a + CR 701.24b: The tutor requires a card and places it
    // on top after one shuffle with no reveal.
    let mut s = table();
    let tutor = card(&mut s, P0, "Cruel Tutor", Zone::Hand);
    let cards = stock(&mut s, P0, &["Hill Giant", "Forest", "Island"]);
    let mut r = s.build();
    announce(&mut r, tutor);
    let mut events = pass_stack(&mut r);
    let WaitingFor::SearchChoice {
        player,
        cards: offered,
        count,
        allows_partial_find,
        ..
    } = &r.state().waiting_for
    else {
        panic!("expected mandatory search, got {:?}", r.state().waiting_for);
    };
    assert_eq!(*player, P0);
    assert_eq!(offered, &cards);
    assert_eq!(*count, 1);
    assert!(!allows_partial_find);
    assert!(r.act(GameAction::SelectCards { cards: vec![] }).is_err());
    events.extend(act(
        &mut r,
        GameAction::SelectCards {
            cards: vec![cards[1]],
        },
    ));
    events.extend(pass_stack(&mut r));
    assert!(cards.iter().all(|&id| reveals(&events, id) == 0));
    assert_eq!(shuffles(&events, P0), 1);
    assert_eq!(library(&r, P0).first(), Some(&cards[1]));
    assert_eq!(r.state().players[0].life, 18);
    settled(&r);
}

// CR 117.3d + CR 805.5b: Pass priority until the requested player or team representative can act.
fn priority(r: &mut GameRunner, player: PlayerId) {
    settled(r);
    for _ in 0..r.state().players.len() {
        if matches!(r.state().waiting_for, WaitingFor::Priority { player: p } if p == player) {
            return;
        }
        act(r, GameAction::PassPriority);
    }
    panic!(
        "expected priority for {player:?}, got {:?}",
        r.state().waiting_for
    );
}

// CR 601.2c + CR 602.2b: Submit the object choice for a spell or activated ability.
fn choose_object(r: &mut GameRunner, id: ObjectId) {
    act(
        r,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(id)),
        },
    );
}

// CR 601.2c + CR 117.4: Choose the spell's target and pass priority to resolve it.
fn cast_at(r: &mut GameRunner, spell: ObjectId, target: ObjectId) {
    r.cast(spell).target_object(target).resolve();
    settled(r);
}

// CR 605.3a: Use a land's mana ability while its controller has priority.
fn tap_land(r: &mut GameRunner, land: ObjectId) {
    let (_, _, by_object) = engine::ai_support::legal_actions_full(r.state());
    let tap = by_object[&land]
        .iter()
        .find(|action| matches!(action, GameAction::TapLandForMana { .. }))
        .expect("printed land can tap for mana");
    act(r, tap.clone());
}

// CR 115.1a + CR 115.1c: Check legal targets when announcing a spell or an activated ability.
fn inspect_targets(
    r: &GameRunner,
    actor: PlayerId,
    spell: ObjectId,
    source: ObjectId,
    host: ObjectId,
    other: ObjectId,
    expected: bool,
) {
    for is_spell in [true, false] {
        let mut probe = GameRunner::from_state(r.state().clone());
        priority(&mut probe, actor);
        if is_spell {
            if probe.state().players[actor.0 as usize]
                .mana_pool
                .count_color(ManaType::Green)
                == 0
            {
                let land = probe
                    .state()
                    .battlefield
                    .iter()
                    .copied()
                    .find(|id| {
                        let obj = &probe.state().objects[id];
                        obj.controller == actor && obj.name == "Forest" && !obj.tapped
                    })
                    .expect("an untapped Forest for Giant Growth");
                tap_land(&mut probe, land);
            }
            announce(&mut probe, spell);
        } else {
            act(
                &mut probe,
                GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                },
            );
        }
        let WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        } = &probe.state().waiting_for
        else {
            panic!(
                "expected a target announcement for {actor:?}, got {:?}",
                probe.state().waiting_for
            );
        };
        let offered = &target_slots[selection.current_slot].legal_targets;
        assert!(offered.contains(&TargetRef::Object(other)));
        assert_eq!(
            offered.contains(&TargetRef::Object(host)),
            expected,
            "actor {actor:?}, spell {is_spell}"
        );
        choose_object(&mut probe, if expected { host } else { other });
        pass_stack(&mut probe);
        settled(&probe);
    }
}

// CR 102.3 + CR 109.5 + CR 611.3b: The Aura restricts its controller's opponents while attached.
fn aura_targeting(name: &str, team: bool, host_controller: PlayerId) {
    use engine::game::game_object::AttachTarget;
    use engine::types::format::FormatConfig;

    let count = if team { 4 } else { 3 };
    let mut s = if team {
        GameScenario::new_with_format(FormatConfig::two_headed_giant(), count, 42)
    } else {
        GameScenario::new_n_player(count, 42)
    };
    s.at_phase(Phase::PreCombatMain);
    let mut seats = Vec::new();
    for index in 0..count {
        let player = PlayerId(index);
        fund(&mut s, player);
        card(&mut s, player, "Forest", Zone::Battlefield);
        card(&mut s, player, "Forest", Zone::Battlefield);
        card(&mut s, player, "Forest", Zone::Battlefield);
        card(&mut s, player, "Island", Zone::Battlefield);
        card(&mut s, player, "Island", Zone::Battlefield);
        let spell = card(&mut s, player, "Giant Growth", Zone::Hand);
        let source = card(&mut s, player, "Prodigal Pyromancer", Zone::Battlefield);
        stock(&mut s, player, &["Swamp", "Plains", "Mountain"]);
        // CR 805.5a + CR 805.5b: Players act and pass through shared team priority. The engine's
        // public priority prompt currently authorizes only the team representative.
        if !team || index % 2 == 0 {
            seats.push((player, spell, source));
        }
    }
    let host = card(&mut s, host_controller, "Grizzly Bears", Zone::Battlefield);
    let other = card(&mut s, P0, "Hill Giant", Zone::Battlefield);
    let aura = card(&mut s, P0, name, Zone::Hand);
    let destroy = card(&mut s, P0, "Naturalize", Zone::Hand);
    let new_controller = PlayerId(2);
    let steal = card(&mut s, new_controller, "Steal Enchantment", Zone::Hand);
    let mut r = s.build();
    for &(player, spell, source) in &seats {
        inspect_targets(&r, player, spell, source, host, other, true);
    }
    cast_at(&mut r, aura, host);
    assert_eq!(
        r.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(host))
    );
    for &(player, spell, source) in &seats {
        let allowed = player == P0;
        inspect_targets(&r, player, spell, source, host, other, allowed);
    }
    if !team && host_controller == P1 {
        for _ in 0..100 {
            if r.state().active_player == new_controller && r.state().phase == Phase::PreCombatMain
            {
                break;
            }
            if matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. }) {
                r.declare_attackers(&[]).expect("skip combat");
            } else {
                act(&mut r, GameAction::PassPriority);
            }
        }
        assert_eq!(r.state().active_player, new_controller);
        assert_eq!(r.state().phase, Phase::PreCombatMain);
        cast_at(&mut r, steal, aura);
        assert_eq!(r.state().objects[&host].controller, host_controller);
        assert_eq!(r.state().objects[&aura].controller, new_controller);
        for &(player, spell, source) in &seats {
            inspect_targets(
                &r,
                player,
                spell,
                source,
                host,
                other,
                player == new_controller,
            );
        }
    }
    priority(&mut r, P0);
    cast_at(&mut r, destroy, aura);
    assert_eq!(r.state().objects[&aura].zone, Zone::Graveyard);
    for (player, spell, source) in seats {
        inspect_targets(&r, player, spell, source, host, other, true);
    }
    settled(&r);
}

#[test]
fn canopy_cover_checks_the_auras_opponents_for_spells_and_abilities() {
    // CR 102.3 + CR 109.5 + CR 115.1a + CR 115.1c + CR 611.3b: Canopy Cover bars spells and
    // abilities from the Aura controller's opponents, including when a teammate controls the host.
    aura_targeting("Canopy Cover", false, P0);
    aura_targeting("Canopy Cover", false, P1);
    aura_targeting("Canopy Cover", true, P1);
    aura_targeting("Canopy Cover", true, PlayerId(2));
}

#[test]
fn shielding_plax_uses_the_same_aura_relative_targeting_rule() {
    // CR 102.3 + CR 109.5 + CR 115.1a + CR 115.1c + CR 611.3b: Shielding Plax applies the same
    // restriction to spells and abilities when a teammate or opponent controls the host.
    aura_targeting("Shielding Plax", false, P1);
    aura_targeting("Shielding Plax", true, P1);
    aura_targeting("Shielding Plax", true, PlayerId(2));
}

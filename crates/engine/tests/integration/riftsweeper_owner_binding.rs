//! Ordinary full-Oracle witnesses for R2 (declared-target owner shuffles), the
//! declaration, owner-continuation and source-owner shuffle spellings
//! (Etrata, Alabaster Dragon, Watery Grasp) and the empty-filter choose guard
//! (Forge of Heroes), independent of Beam casting.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::{CastChoice, GameAction};
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const RIFTSWEEPER: &str = "When this creature enters, choose target face-up exiled card. Its owner shuffles it into their library.";
const ETRATA_THE_SILENCER: &str = "Etrata can't be blocked.\nWhenever Etrata deals combat damage to a player, exile target creature that player controls and put a hit counter on that card. That player loses the game if they own three or more exiled cards with hit counters on them. Etrata's owner shuffles Etrata into their library.";
const ALABASTER_DRAGON: &str =
    "Flying\nWhen this creature dies, shuffle it into its owner's library.";
const MURDER: &str = "Destroy target creature.";
const WRATH_OF_GOD: &str = "Destroy all creatures. They can't be regenerated.";
const WATERY_GRASP: &str = "Enchant creature\nEnchanted creature doesn't untap during its controller's untap step.\nWaterbend {5}: Enchanted creature's owner shuffles it into their library. (While paying a waterbend cost, you can tap your artifacts and creatures to help. Each one pays for {1}.)";
const FORGE_OF_HEROES: &str = "{T}: Add {C}.\n{T}: Choose target commander that entered this turn. Put a +1/+1 counter on it if it's a creature and a loyalty counter on it if it's a planeswalker.";
const OBLATION: &str =
    "The owner of target nonland permanent shuffles it into their library, then draws two cards.";
const ZOYOWAS_JUSTICE: &str = "The owner of target artifact or creature with mana value 1 or greater shuffles it into their library. Then that player discovers X, where X is its mana value. (They exile cards from the top of their library until they exile a nonland card with that mana value or less. They cast it without paying its mana cost or put it into their hand. They put the rest on the bottom in a random order.)";
const THIS_IS_HOW_IT_ENDS: &str = "Target creature's owner shuffles it into their library, then faces a villainous choice \u{2014} They lose 5 life, or they shuffle another creature they own into their library.";
const TURN_THE_EARTH: &str = "Choose up to three target cards in graveyards. The owners of those cards shuffle them into their libraries. You gain 2 life.\nFlashback {1}{G} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";

fn finished(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
}

fn shuffles(events: &[GameEvent]) -> Vec<PlayerId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect()
}

/// The players whose library was shuffled, each listed once, in first-shuffle
/// order. A single-object move into a library followed by the printed
/// "shuffle" step emits one `ShuffledLibrary` event per step (the zone move's
/// auto-shuffle convention, then the explicit `Effect::Shuffle`); this reads
/// WHOSE library, which is what the owner-binding rows discriminate, and
/// is deliberately blind to that pre-existing double event.
fn distinct_shuffles(events: &[GameEvent]) -> Vec<PlayerId> {
    let mut players = shuffles(events);
    players.dedup();
    players
}

// CR 406.3 + CR 608.2b: only face-up exile is a target, and a new incarnation
// cannot replace the selected card. CR 701.24a: the owner shuffles exactly once.
#[test]
fn riftsweeper_face_up_target_owner_and_stale_incarnation() {
    for stale in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_to_hand(P0, "Riftsweeper", 2, 2)
            .with_subtypes(vec!["Elf", "Shaman"])
            .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
            .from_oracle_text(RIFTSWEEPER)
            .id();
        let target = scenario
            .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
            .id();
        let other = scenario
            .add_creature_to_graveyard(P0, "Balduvian Bears", 2, 2)
            .id();
        let hidden = scenario
            .add_creature_to_graveyard(P1, "Runeclaw Bear", 2, 2)
            .id();
        let grave = scenario
            .add_creature_to_graveyard(P1, "Forest Bear", 2, 2)
            .id();
        scenario.with_mana_pool(
            P0,
            (0..2)
                .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        for id in [target, other, hidden] {
            engine::game::zones::move_to_zone(runner.state_mut(), id, Zone::Exile, &mut vec![]);
        }
        runner
            .state_mut()
            .objects
            .get_mut(&hidden)
            .unwrap()
            .face_down = true;
        runner.cast(source).commit();
        runner.advance_until_stack_empty();
        let WaitingFor::TriggerTargetSelection {
            player,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "Riftsweeper must announce one exile target: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(target_slots.len(), 1);
        assert_eq!(selection.current_slot, 0);
        assert!(!target_slots[0].optional);
        assert_eq!(target_slots[0].legal_targets.len(), 2);
        for id in [target, other] {
            assert!(target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(id)));
        }
        for id in [hidden, grave] {
            assert!(!target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(id)));
        }
        assert!(engine::ai_support::legal_actions_for_viewer(runner.state(), P0).0.iter().any(|action| matches!(action, GameAction::ChooseTarget { target: Some(TargetRef::Object(id)) } if *id == target)));
        assert!(
            engine::ai_support::legal_actions_for_viewer(runner.state(), P1)
                .0
                .is_empty()
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .unwrap();
        assert_eq!(runner.state().stack.len(), 1);
        if stale {
            let incarnation = runner.state().objects[&target].incarnation;
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                target,
                Zone::Graveyard,
                &mut vec![],
            );
            engine::game::zones::move_to_zone(runner.state_mut(), target, Zone::Exile, &mut vec![]);
            assert_ne!(runner.state().objects[&target].incarnation, incarnation);
        }
        let mut events = vec![];
        for _ in 0..2 {
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        finished(&runner);
        assert_eq!(shuffles(&events), if stale { vec![] } else { vec![P1] });
        assert_eq!(
            runner.state().objects[&target].zone,
            if stale { Zone::Exile } else { Zone::Library }
        );
        assert_eq!(runner.state().players[1].library.contains(&target), !stale);
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&other].zone, Zone::Exile);
        assert_eq!(runner.state().objects[&hidden].zone, Zone::Exile);
        assert_eq!(runner.state().objects[&grave].zone, Zone::Graveyard);
    }
}

// CR 608.2b: the mandatory trigger cannot acquire the source when exile is empty.
#[test]
fn riftsweeper_without_face_up_exile_has_no_owner_action() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_to_hand(P0, "Riftsweeper", 2, 2)
        .with_subtypes(vec!["Elf", "Shaman"])
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
        .from_oracle_text(RIFTSWEEPER)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(source).resolve();
    assert_eq!(outcome.zone_of(source), Zone::Battlefield);
    assert!(shuffles(outcome.events()).is_empty());
    finished(&runner);
}

// CR 701.24a: complete successive spells must publish only their own owners.
#[test]
fn turn_the_earth_successive_owner_populations_keep_life_tail() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
        .id();
    let second = scenario
        .add_creature_to_graveyard(P0, "Runeclaw Bear", 2, 2)
        .id();
    let spells: Vec<_> = (0..3)
        .map(|_| {
            scenario
                .add_spell_to_hand(P0, "Turn the Earth", true)
                .with_mana_cost(parse_mtgjson_mana_cost("{G}"))
                .from_oracle_text_with_keywords(&["Flashback"], TURN_THE_EARTH)
                .id()
        })
        .collect();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    for (spell, target, owner) in [
        (spells[0], Some(first), Some(P1)),
        (spells[1], Some(second), Some(P0)),
        (spells[2], None, None),
    ] {
        let cast = runner.cast(spell);
        let outcome = if let Some(target) = target {
            cast.target_object(target).resolve()
        } else {
            cast.resolve()
        };
        assert_eq!(
            shuffles(outcome.events()),
            owner.into_iter().collect::<Vec<_>>()
        );
        outcome.assert_life_delta(P0, 2);
        if let Some(target) = target {
            outcome.assert_zone(&[target], Zone::Library);
        }
        finished(&runner);
    }
    assert_eq!(runner.life(P0), 26);
}

// CR 608.2c: preserve the reveal/deploy continuation after the owner's shuffle.
#[test]
fn chaos_warp_owner_shuffle_reveals_and_deploys_the_permanent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .controlled_by(P0)
        .id();
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Chaos Warp", true,
        "The owner of target permanent shuffles it into their library, then reveals the top card of their library. If it's a permanent card, they put it onto the battlefield.")
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{R}")).id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(target).resolve();
    assert_eq!(shuffles(outcome.events()), vec![P1]);
    assert!(
        outcome.events().iter().any(|event| matches!(event,
            GameEvent::CardsRevealed { player: P1, card_ids, .. } if card_ids == &[target]
        )),
        "Chaos Warp must reveal the only card in the target owner's library"
    );
    assert_eq!(outcome.state().last_revealed_ids, vec![target]);
    outcome.assert_zone(&[target], Zone::Battlefield);
    assert_eq!(outcome.state().objects[&target].controller, P1);
    assert!(outcome.events().iter().any(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Library, .. } if *object_id == target)));
    assert!(outcome.events().iter().any(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: Some(Zone::Library), to: Zone::Battlefield, .. } if *object_id == target)));
    finished(&runner);
}

// CR 701.24d: a designated player's empty graveyard still causes a shuffle.
#[test]
fn clear_the_mind_preserves_target_player_and_empty_mass_population() {
    for populated in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let card = populated.then(|| {
            scenario
                .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
                .id()
        });
        let draw = scenario.add_card_to_library_top(P0, "Island");
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Clear the Mind",
                false,
                "Target player shuffles their graveyard into their library.\nDraw a card.",
            )
            .with_mana_cost(parse_mtgjson_mana_cost("{2}{U}"))
            .id();
        scenario.with_mana_pool(
            P0,
            (0..3)
                .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).target_player(P1).resolve();
        assert_eq!(shuffles(outcome.events()), vec![P1]);
        outcome.assert_zone(&[draw], Zone::Hand);
        if let Some(card) = card {
            outcome.assert_zone(&[card], Zone::Library);
        }
        finished(&runner);
    }
}

// CR 601.2c + CR 608.2d: damage is targeted at announcement; the controller
// chooses untargeted lands only when the spell resolves, after dealing damage.
#[test]
fn worldsouls_rage_keeps_damage_target_and_resolution_land_choice_separate() {
    for (has_hand, has_grave, decline) in [
        (true, false, false),
        (false, true, false),
        (true, true, false),
        (true, true, true),
        (false, false, false),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let hand = has_hand.then(|| scenario.add_basic_land(P0, ManaColor::Green));
        let grave = has_grave.then(|| scenario.add_basic_land(P0, ManaColor::Red));
        let opponent = scenario.add_basic_land(P1, ManaColor::Blue);
        let nonland = scenario
            .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
            .with_subtypes(vec!["Bear"])
            .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
            .from_oracle_text("")
            .id();
        let spell = scenario.add_spell_to_hand_from_oracle(P0, "Worldsoul's Rage", false,
            "Worldsoul's Rage deals X damage to any target. Put up to X land cards from your hand and/or graveyard onto the battlefield tapped.")
            .with_mana_cost(parse_mtgjson_mana_cost("{X}{R}{G}")).id();
        scenario.with_mana_pool(
            P0,
            [
                ManaType::Red,
                ManaType::Green,
                ManaType::Green,
                ManaType::Green,
            ]
            .into_iter()
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
        );
        let mut runner = scenario.build();
        for (id, zone) in hand
            .into_iter()
            .map(|id| (id, Zone::Hand))
            .chain(grave.into_iter().map(|id| (id, Zone::Graveyard)))
            .chain([(opponent, Zone::Graveyard)])
        {
            engine::game::zones::move_to_zone(runner.state_mut(), id, zone, &mut vec![]);
        }
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseXValue { player: P0, .. }
        ));
        runner.act(GameAction::ChooseX { value: 2 }).unwrap();
        let WaitingFor::TargetSelection {
            player,
            target_slots,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "damage must have an announcement target: {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(*player, P0);
        assert_eq!(target_slots.len(), 1);
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Player(P1)));
        assert!(!target_slots[0].optional);
        for id in hand.into_iter().chain(grave).chain([opponent, nonland]) {
            assert!(!target_slots[0]
                .legal_targets
                .contains(&TargetRef::Object(id)));
        }
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            })
            .unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.life(P1), 20);
        runner.advance_until_stack_empty();
        assert_eq!(runner.life(P1), 18);
        let expected: Vec<_> = hand.into_iter().chain(grave).collect();
        if !expected.is_empty() {
            let WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                min_count,
                up_to,
                ..
            } = &runner.state().waiting_for
            else {
                panic!(
                    "lands must be chosen on resolution: {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(*player, P0);
            assert_eq!(*count, expected.len());
            assert_eq!(*min_count, 0);
            assert!(*up_to);
            assert_eq!(cards.len(), expected.len());
            assert!(expected.iter().all(|id| cards.contains(id)));
            assert!(!cards.contains(&opponent) && !cards.contains(&nonland));
            assert!(
                engine::ai_support::legal_actions_for_viewer(runner.state(), P1)
                    .0
                    .is_empty()
            );
            let result = runner
                .act(GameAction::SelectCards {
                    cards: if decline { vec![] } else { expected.clone() },
                })
                .unwrap();
            if !decline {
                for (id, origin) in hand
                    .into_iter()
                    .map(|id| (id, Zone::Hand))
                    .chain(grave.into_iter().map(|id| (id, Zone::Graveyard)))
                {
                    assert!(result.events.iter().any(|event| matches!(event,
                        GameEvent::ZoneChanged { object_id, from, to: Zone::Battlefield, .. }
                        if *object_id == id && *from == Some(origin))));
                }
            }
        }
        runner.advance_until_stack_empty();
        finished(&runner);
        for (id, origin) in hand
            .into_iter()
            .map(|id| (id, Zone::Hand))
            .chain(grave.into_iter().map(|id| (id, Zone::Graveyard)))
        {
            assert_eq!(
                runner.state().objects[&id].zone,
                if decline { origin } else { Zone::Battlefield }
            );
            if !decline {
                assert!(runner.state().objects[&id].tapped);
            }
        }
        assert_eq!(runner.life(P1), 18);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&opponent].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&nonland].zone, Zone::Graveyard);
    }
}

/// The legal targets of the slot the runner is currently asking about
/// (`WaitingFor::TargetSelection` / `TriggerTargetSelection`).
fn current_slot_legal_targets(runner: &GameRunner) -> Vec<TargetRef> {
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        }
        | WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        } => target_slots
            .get(selection.current_slot)
            .map(|slot| slot.legal_targets.clone())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Drive an attack by `attacker` at `defender` through combat damage and every
/// resulting trigger to a quiet `Priority` window. A target prompt is answered
/// with `pick` (and the prompt's legal set is recorded in `offered`).
fn attack_and_settle(
    runner: &mut GameRunner,
    attacker: ObjectId,
    defender: PlayerId,
    pick: ObjectId,
    offered: &mut Vec<TargetRef>,
) -> Vec<GameEvent> {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(defender))])
        .expect("the attacker is legal");
    let mut events = runner.combat_damage().events().to_vec();
    for _ in 0..40 {
        let result = match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                *offered = current_slot_legal_targets(runner);
                runner.act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(pick)),
                })
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            other => panic!("unexpected prompt while settling combat: {other:?}"),
        };
        events.extend(result.expect("prompt answer accepted").events);
    }
    events
}

// CR 201.5 + CR 400.3 + CR 701.24a: "Etrata's owner shuffles Etrata into their
// library" moves Etrata herself, not the earlier declared exile target, to HER
// owner's library, and that library is shuffled. Etrata is owned by P2 and
// controlled by P0; she attacks P1; the declared exile target is a creature
// owned by P2 and controlled by P1 (CR 603.3a: the trigger's controller, P0,
// announces it).
//
// The earlier exile target is owned by Etrata's owner on purpose. Separate,
// pre-existing card bug (it reproduces on main with Beacon of Destruction cast
// at another player's creature, and is described in the PR body): when the
// earlier declared target's owner differs from the source's owner, the
// terminal shuffle resolves to the target's owner. This row does not
// discriminate the target's owner from the source's owner (equal by
// construction) and makes no claim across that defect.
//
// Not asserted: P1's game state. The "loses the game if ..." sentence printed
// before the shuffle is still read as an unconditional loss (a separate,
// pre-existing misparse), so P1 may have left the game (CR 800.4a); the row does
// not depend on it. The P0 and P1 negatives are paired with the positive P2
// shuffle, which is the reach guard when P1 has left.
#[test]
fn etrata_source_owner_shuffle_moves_etrata_to_its_owners_library() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let etrata = scenario
        .add_creature(P2, "Etrata, the Silencer", 2, 3)
        .with_subtypes(vec!["Vampire", "Assassin"])
        .controlled_by(P0)
        .from_oracle_text(ETRATA_THE_SILENCER)
        .id();
    let victim = scenario
        .add_creature(P2, "Grizzly Bears", 2, 2)
        .controlled_by(P1)
        .id();
    scenario.with_library_top(P0, &["P0 Card"]);
    scenario.with_library_top(P1, &["P1 Card"]);
    scenario.with_library_top(P2, &["P2 Card"]);
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&etrata].owner, P2);
    assert_eq!(runner.state().objects[&etrata].controller, P0);
    assert_eq!(runner.state().objects[&victim].owner, P2);
    assert_eq!(runner.state().objects[&victim].controller, P1);

    let mut offered = vec![];
    let events = attack_and_settle(&mut runner, etrata, P1, victim, &mut offered);

    // Reach guard: the declared-target exile and the hit counter resolved.
    assert_eq!(runner.state().objects[&victim].zone, Zone::Exile);
    assert_eq!(
        runner.state().objects[&victim]
            .counters
            .get(&CounterType::Generic("hit".to_string()))
            .copied(),
        Some(1),
        "the exiled card carries one hit counter"
    );
    // CR 201.5 + CR 400.3: Etrata herself is in her OWNER's library.
    assert_eq!(runner.state().objects[&etrata].zone, Zone::Library);
    assert!(runner.state().players[2].library.contains(&etrata));
    assert!(!runner.state().players[0].library.contains(&etrata));
    // CR 701.24a: P2's library is shuffled; neither the controller's (P0) nor
    // the damaged player's (P1) is.
    assert_eq!(distinct_shuffles(&events), vec![P2]);
    finished(&runner);
}

/// Pass priority until the stack is empty, returning every emitted event.
fn pass_until_stack_empty(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = vec![];
    for _ in 0..6 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("priority passes while the spell resolves")
                .events,
        );
    }
    events
}

// CR 608.2c + CR 108.3: "The owner of target nonland permanent shuffles it into
// their library, then draws two cards." The permanent is owned by P1 and
// controlled by P0, so the OWNER (P1) shuffles and draws; the caster draws
// nothing. P1's library holds enough cards that two draws never meet an empty
// library (CR 121.4).
#[test]
fn oblation_owner_shuffles_then_draws_two_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .controlled_by(P0)
        .id();
    scenario.with_library_top(P0, &["P0 Card A", "P0 Card B", "P0 Card C"]);
    scenario.with_library_top(P1, &["P1 Card A", "P1 Card B", "P1 Card C", "P1 Card D"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Oblation", true, OBLATION)
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{W}"))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&target].owner, P1);
    assert_eq!(runner.state().objects[&target].controller, P0);

    let outcome = runner.cast(spell).target_object(target).resolve();

    // Reach guard: the target went into its owner's library (it may then be one
    // of the two cards the owner draws) and that library was shuffled.
    assert!(outcome.events().iter().any(|event| matches!(event,
        GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Library, .. }
        if *object_id == target)));
    assert_eq!(distinct_shuffles(outcome.events()), vec![P1]);
    // The OWNER draws two cards; the caster draws none.
    assert_eq!(outcome.hand_drawn(P1), 2);
    assert_eq!(outcome.hand_drawn(P0), 0);
    finished(&runner);
}

// CR 608.2c + CR 108.3 + CR 701.57a + CR 601.2a: "The owner of target artifact or
// creature with mana value 1 or greater shuffles it into their library. Then
// that player discovers X, where X is its mana value." Grizzly Bears (mana value
// 2) is owned by P1, so P1 discovers 2 from P1's library: the offer is addressed
// to P1, the Bears (the only nonland card there) are cast by P1 and enter under
// P1's control, and P0's library is untouched. P1's library is staged with typed
// lands, so every card passed over on the way to the hit is a land.
#[test]
fn zoyowas_justice_owner_discovers_from_their_own_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
        .id();
    scenario.add_land_to_library_top(P1, "Forest");
    scenario.add_land_to_library_top(P1, "Forest");
    let p0_top = scenario
        .add_spell_to_library_top(P0, "P0 Hit", false)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}"))
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Zoyowa's Justice", true, ZOYOWAS_JUSTICE)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{R}"))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&target].owner, P1);

    let mut events = runner
        .cast(spell)
        .target_object(target)
        .commit()
        .events()
        .to_vec();
    for _ in 0..2 {
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }

    // The cast-or-hand offer is the owner's, for the Bears discovered from P1's
    // library (reach guard: the discover hit the target card).
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Discover { hit_card, discover_value: 2, .. },
                ..
            } if hit_card == target
        ),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().waiting_for.acting_player(),
        Some(P1),
        "the discovering owner (P1) decides whether to cast the hit"
    );
    assert!(events.iter().any(|event| matches!(event,
        GameEvent::ZoneChanged { object_id, from: Some(Zone::Library), to: Zone::Exile, .. }
        if *object_id == target)));

    events.extend(
        runner
            .act(GameAction::DiscoverChoice {
                choice: CastChoice::Cast,
            })
            .expect("the owner casts the discovered card")
            .events,
    );
    events.extend(pass_until_stack_empty(&mut runner));

    assert!(
        events.iter().any(|event| matches!(event,
            GameEvent::SpellCast { object_id, controller: P1, .. } if *object_id == target)),
        "the Bears are cast by their owner P1"
    );
    assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&target].controller, P1);
    // Reach guard + negative: P1's library was shuffled (twice at most, never
    // P0's), and P0's library top was not exiled by P0's discover.
    assert!(distinct_shuffles(&events).contains(&P1));
    assert!(!distinct_shuffles(&events).contains(&P0));
    assert_eq!(runner.state().objects[&p0_top].zone, Zone::Library);
    assert!(runner.state().players[0].library.contains(&p0_top));
    finished(&runner);
}

// CR 108.3 + CR 701.55a + CR 608.2c: "Target creature's owner shuffles it into
// their library, then faces a villainous choice". The creature is owned by P1
// and controlled by P0 (the caster), so the OWNER (P1) faces the choice and, on
// the lose-5-life branch, is the player who loses the life.
#[test]
fn this_is_how_it_ends_owner_not_controller_faces_the_villainous_choice() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Card A", "P0 Card B", "P0 Card C"]);
    scenario.with_library_top(P1, &["P1 Card A", "P1 Card B", "P1 Card C"]);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    let target = scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .controlled_by(P0)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "This Is How It Ends", true, THIS_IS_HOW_IT_ENDS)
        .with_mana_cost(parse_mtgjson_mana_cost("{3}{B}"))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&target].owner, P1);
    assert_eq!(runner.state().objects[&target].controller, P0);

    let mut events = runner
        .cast(spell)
        .target_object(target)
        .commit()
        .events()
        .to_vec();
    for _ in 0..2 {
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }

    // Reach guard: the shuffle resolved before the choice was offered.
    assert_eq!(distinct_shuffles(&events), vec![P1]);
    assert_eq!(runner.state().objects[&target].zone, Zone::Library);
    let WaitingFor::ChooseOneOfBranch {
        player, branches, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected the villainous ChooseOneOfBranch: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1, "the target's owner faces the choice");
    assert_eq!(runner.state().waiting_for.acting_player(), Some(P1));
    let lose_life = branches
        .iter()
        .position(|branch| {
            matches!(
                &*branch.effect,
                engine::types::ability::Effect::LoseLife { .. }
            )
        })
        .expect("a 'They lose 5 life' branch exists");

    runner
        .act(GameAction::ChooseBranch { index: lose_life })
        .expect("the lose-life branch resolves");
    assert_eq!(runner.life(P1), 15, "the owner loses 5 life");
    assert_eq!(runner.life(P0), 20, "the caster loses no life");
    finished(&runner);
}

// CR 400.7e + CR 400.3 + CR 701.24a: "When this creature dies, shuffle it into
// its owner's library." The Dragon is owned by P1 and controlled by P0, so the
// death trigger is P0's, but the card goes to P1's library and P1's library is
// shuffled. The trigger declares no target (CR 115.10a).
#[test]
fn alabaster_dragon_death_shuffles_it_into_its_owners_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragon = scenario
        .add_creature(P1, "Alabaster Dragon", 4, 4)
        .controlled_by(P0)
        .with_subtypes(vec!["Dragon"])
        .from_oracle_text_with_keywords(&["Flying"], ALABASTER_DRAGON)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{B}{B}"))
        .id();
    scenario.with_library_top(P0, &["P0 Card"]);
    scenario.with_library_top(P1, &["P1 Card"]);
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&dragon].owner, P1);
    assert_eq!(runner.state().objects[&dragon].controller, P0);

    let outcome = runner.cast(murder).target_object(dragon).resolve();

    // Reach guard: the Dragon died (battlefield to graveyard) before the
    // trigger moved it on.
    assert!(outcome.events().iter().any(|event| matches!(event,
        GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, .. }
        if *object_id == dragon)));
    outcome.assert_zone(&[dragon], Zone::Library);
    assert!(outcome.state().players[1].library.contains(&dragon));
    assert!(!outcome.state().players[0].library.contains(&dragon));
    assert_eq!(distinct_shuffles(outcome.events()), vec![P1]);
    finished(&runner);
}

// CR 115.10a + CR 108.3 + CR 400.3 + CR 701.24a: "Enchanted creature's owner
// shuffles it into their library" affects the enchanted creature without
// targeting it, so no target prompt arises and the decoy (another P1 creature)
// is untouched. The host goes to its owner's (P1's) library and P1 shuffles.
// CR 704.5m: the Aura then has nothing to enchant and goes to the graveyard.
#[test]
fn watery_grasp_waterbend_shuffles_the_enchanted_creature_without_a_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let decoy = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    let grasp = scenario
        .add_spell_to_hand(P0, "Watery Grasp", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_mana_cost(parse_mtgjson_mana_cost("{U}"))
        .with_color(vec![ManaColor::Blue])
        .from_oracle_text_with_keywords(&["Enchant", "Waterbend"], WATERY_GRASP)
        .id();
    scenario.with_library_top(P1, &["P1 Card"]);
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();

    runner.cast(grasp).target_object(host).resolve();
    // Reach guard: the Aura resolved onto the host.
    assert_eq!(runner.state().objects[&grasp].zone, Zone::Battlefield);
    assert!(runner.state().objects[&grasp].attached_to.is_some());

    let mut events = runner
        .act(GameAction::ActivateAbility {
            source_id: grasp,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the Waterbend activation is announced")
        .events;
    // CR 701.67a + CR 601.2h: the Waterbend cost opens its payment window; with
    // no creatures or artifacts tapped, the pool pays the whole {5}.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment {
                convoke_mode: Some(_),
                ..
            }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    events.extend(
        runner
            .act(GameAction::PassPriority)
            .expect("the pool pays Waterbend {5}")
            .events,
    );
    // CR 115.10a: the host is affected, not targeted, so the activation opens
    // no target prompt and the ability is on the stack.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().stack.len(), 1);
    for _ in 0..4 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }

    assert_eq!(runner.state().objects[&host].zone, Zone::Library);
    assert!(runner.state().players[1].library.contains(&host));
    assert_eq!(runner.state().objects[&decoy].zone, Zone::Battlefield);
    assert_eq!(distinct_shuffles(&events), vec![P1]);
    assert_eq!(runner.state().objects[&grasp].zone, Zone::Graveyard);
    finished(&runner);
}

// CR 115.1 + CR 115.10a: "Choose target commander that entered this turn" is a
// declared target whose filter has no card type and no controller, only the
// commander and entered-this-turn properties. The choose guard must not read
// that as an empty filter: the legal set is exactly the commanders that entered
// this turn (both players', CR 903.3), not the non-commander that entered this
// turn and not the commander that entered earlier.
//
// Honesty guard: the "+1/+1 counter ... loyalty counter" tail is still an
// unsupported clause, so the row asserts no counter placement.
#[test]
fn forge_of_heroes_choose_target_commander_offers_exactly_the_entered_commanders() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let forge = scenario
        .add_land_from_oracle(P0, "Forge of Heroes", FORGE_OF_HEROES)
        .id();
    let mine = scenario
        .add_creature(P0, "Entered Commander A", 2, 2)
        .commander()
        .with_summoning_sickness()
        .id();
    let theirs = scenario
        .add_creature(P1, "Entered Commander B", 2, 2)
        .commander()
        .with_summoning_sickness()
        .id();
    let non_commander = scenario
        .add_creature(P0, "Entered Non-Commander", 2, 2)
        .with_summoning_sickness()
        .id();
    let veteran = scenario
        .add_creature(P1, "Earlier Commander", 2, 2)
        .commander()
        .id();
    let mut runner = scenario.build();
    let earlier_turn = runner.state().turn_number - 1;
    runner
        .state_mut()
        .objects
        .get_mut(&veteran)
        .unwrap()
        .entered_battlefield_turn = Some(earlier_turn);

    runner
        .act(GameAction::ActivateAbility {
            source_id: forge,
            ability_index: 1,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Forge's second ability is activatable");

    // Reach guard: the activation opened a target prompt for the declared slot.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "{:?}",
        runner.state().waiting_for
    );
    let mut legal = current_slot_legal_targets(&runner);
    legal.sort_by_key(|target| match target {
        TargetRef::Object(id) => id.0,
        TargetRef::Player(player) => u64::from(player.0) + u64::from(u32::MAX),
    });
    let mut expected = vec![TargetRef::Object(mine), TargetRef::Object(theirs)];
    expected.sort_by_key(|target| match target {
        TargetRef::Object(id) => id.0,
        TargetRef::Player(player) => u64::from(player.0) + u64::from(u32::MAX),
    });
    assert_eq!(legal, expected);
    for excluded in [non_commander, veteran] {
        assert!(!legal.contains(&TargetRef::Object(excluded)));
    }

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(mine)),
        })
        .expect("an entered commander is a legal choice");
    runner.advance_until_stack_empty();
    finished(&runner);
}

// CR 608.2b + CR 701.24a: Turn the Earth's "up to three target cards" with two
// declared targets, one of which is no longer in a graveyard when the spell
// resolves. The spell still resolves for the legal target: its owner (P1)
// shuffles it into their library; the illegal target (P0's card, now in exile)
// is untouched and P0's library is not shuffled on its account; and the
// untargeted "You gain 2 life" tail still happens.
//
// The owner assertion (`vec![P1]`: the legal target's owner, never the caster P0
// who owns the illegal card) is the part tied to the owner-population binding.
#[test]
fn turn_the_earth_with_one_illegal_target_shuffles_only_the_legal_one_and_keeps_the_tail() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let legal = scenario
        .add_creature_to_graveyard(P1, "Grizzly Bears", 2, 2)
        .id();
    let doomed = scenario
        .add_creature_to_graveyard(P0, "Runeclaw Bear", 2, 2)
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "Turn the Earth", true)
        .with_mana_cost(parse_mtgjson_mana_cost("{G}"))
        .from_oracle_text_with_keywords(&["Flashback"], TURN_THE_EARTH)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    let committed = runner
        .cast(spell)
        .target_objects(&[legal, doomed])
        .commit()
        .stop_at_optional()
        .events()
        .to_vec();
    // Reach guard: both targets were declared and the spell is on the stack.
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);

    // In response, P0's card leaves the graveyard (CR 608.2b: now illegal).
    engine::game::zones::move_to_zone(runner.state_mut(), doomed, Zone::Exile, &mut vec![]);
    assert_eq!(runner.state().objects[&doomed].zone, Zone::Exile);

    let mut events = committed;
    events.extend(pass_until_stack_empty(&mut runner));
    finished(&runner);

    // Reach guards: the legal target was shuffled into its owner's library and
    // the life tail resolved.
    assert_eq!(distinct_shuffles(&events), vec![P1]);
    assert_eq!(runner.state().objects[&legal].zone, Zone::Library);
    assert!(runner.state().players[1].library.contains(&legal));
    assert_eq!(runner.life(P0), life_before + 2, "You gain 2 life");

    // The illegal target is untouched: still in exile, not in any library.
    assert_eq!(runner.state().objects[&doomed].zone, Zone::Exile);
    assert!(!runner.state().players[0].library.contains(&doomed));
    assert!(!runner.state().players[1].library.contains(&doomed));
}

// CR 603.10a + CR 400.3 + CR 701.24a: two Alabaster Dragons die at the same
// time to one board wipe (Wrath of God). Both are controlled by P0 but owned by
// different players (P0 and P1). Each death triggers (look-back, CR 603.10a),
// and each trigger shuffles ITS Dragon into ITS OWNER's library, shuffling
// that owner's library.
//
// Guards the simultaneous-death case: one trigger per Dragon, each Dragon in its
// own owner's library. This row was not run against reverted production code.
#[test]
fn alabaster_dragons_dying_together_each_shuffle_into_their_owners_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let own_dragon = scenario
        .add_creature(P0, "Alabaster Dragon", 4, 4)
        .with_subtypes(vec!["Dragon"])
        .from_oracle_text_with_keywords(&["Flying"], ALABASTER_DRAGON)
        .id();
    let borrowed_dragon = scenario
        .add_creature(P1, "Alabaster Dragon", 4, 4)
        .controlled_by(P0)
        .with_subtypes(vec!["Dragon"])
        .from_oracle_text_with_keywords(&["Flying"], ALABASTER_DRAGON)
        .id();
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{W}{W}"))
        .id();
    scenario.with_library_top(P0, &["P0 Card"]);
    scenario.with_library_top(P1, &["P1 Card"]);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&own_dragon].owner, P0);
    assert_eq!(runner.state().objects[&borrowed_dragon].owner, P1);
    assert_eq!(runner.state().objects[&borrowed_dragon].controller, P0);

    let mut events = runner.cast(wrath).commit().events().to_vec();
    // Pass priority until Wrath has resolved and both death triggers wait.
    for _ in 0..4 {
        if runner.state().stack.len() >= 2 {
            break;
        }
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }

    // Reach guards: both Dragons died in the same wipe and both death triggers
    // are on the stack together.
    for dragon in [own_dragon, borrowed_dragon] {
        assert!(
            events.iter().any(|event| matches!(event,
                GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, .. }
                if *object_id == dragon)),
            "{dragon:?} died to the wipe"
        );
    }
    assert_eq!(
        runner.state().stack.len(),
        2,
        "one death trigger per Dragon is on the stack"
    );

    events.extend(pass_until_stack_empty(&mut runner));
    finished(&runner);

    // Each Dragon is in ITS OWNER's library, and each owner's library was
    // shuffled.
    assert_eq!(runner.state().objects[&own_dragon].zone, Zone::Library);
    assert!(runner.state().players[0].library.contains(&own_dragon));
    assert!(!runner.state().players[1].library.contains(&own_dragon));
    assert_eq!(runner.state().objects[&borrowed_dragon].zone, Zone::Library);
    assert!(runner.state().players[1].library.contains(&borrowed_dragon));
    assert!(!runner.state().players[0].library.contains(&borrowed_dragon));
    let mut shuffled = distinct_shuffles(&events);
    shuffled.sort();
    shuffled.dedup();
    assert_eq!(
        shuffled,
        vec![P0, P1],
        "both owners' libraries are shuffled"
    );
}

const CATHARTIC_PARTING: &str = "The owner of target artifact or enchantment an opponent controls shuffles it into their library. You may shuffle up to four target cards from your graveyard into your library.";
const FLAME_SLASH: &str = "Flame Slash deals 4 damage to target creature.";
const SOL_RING: &str = "{T}: Add {C}{C}.";

/// Pass priority until the stack is empty, answering every optional-effect
/// prompt (CR 608.2d) with `accept` through the real `DecideOptionalEffect`
/// action. Returns every emitted event and how many optional prompts were
/// answered.
fn settle_answering_optionals(runner: &mut GameRunner, accept: bool) -> (Vec<GameEvent>, usize) {
    let mut events = vec![];
    let mut prompts = 0;
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(*player, P0, "the caster makes the 'you may' choice");
                prompts += 1;
                events.extend(
                    runner
                        .act(GameAction::DecideOptionalEffect { accept })
                        .expect("the optional-effect answer is accepted")
                        .events,
                );
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => events.extend(
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes while the spell resolves")
                    .events,
            ),
            other => panic!("unexpected prompt while resolving Cathartic Parting: {other:?}"),
        }
    }
    (events, prompts)
}

// CR 608.2b + CR 701.24a + CR 108.3: Cathartic Parting has two separate target
// groups ("target artifact or enchantment an opponent controls" and "up to four
// target cards from your graveyard"). The artifact is OWNED by P2 and controlled
// by P1 (an opponent of the caster P0), so the owner binding is observable. When
// only one group's targets are illegal at resolution, the other group's part
// still happens and the illegal group is untouched.
#[test]
fn cathartic_parting_resolves_the_legal_target_group_when_the_other_group_is_illegal() {
    for first_group_illegal in [true, false] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let artifact = scenario
            .add_artifact_from_oracle(P2, "Sol Ring", SOL_RING)
            .controlled_by(P1)
            .id();
        let grave_a = scenario
            .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
            .id();
        let grave_b = scenario
            .add_creature_to_graveyard(P0, "Runeclaw Bear", 2, 2)
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Cathartic Parting", false, CATHARTIC_PARTING)
            .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
            .id();
        scenario.with_library_top(P0, &["P0 Card"]);
        scenario.with_library_top(P1, &["P1 Card"]);
        scenario.with_library_top(P2, &["P2 Card"]);
        scenario.with_mana_pool(
            P0,
            (0..2)
                .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        assert_eq!(runner.state().objects[&artifact].owner, P2);
        assert_eq!(runner.state().objects[&artifact].controller, P1);

        let mut events = runner
            .cast(spell)
            .target_objects(&[artifact, grave_a, grave_b])
            .commit()
            .events()
            .to_vec();
        // Reach guard: all three targets were declared and the spell is on the
        // stack, awaiting resolution.
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);

        // In response, one whole target group leaves its zone (CR 608.2b).
        let doomed: &[ObjectId] = if first_group_illegal {
            &[artifact]
        } else {
            &[grave_a, grave_b]
        };
        for id in doomed {
            engine::game::zones::move_to_zone(runner.state_mut(), *id, Zone::Exile, &mut vec![]);
            assert_eq!(runner.state().objects[id].zone, Zone::Exile);
        }

        let (resolved, prompts) = settle_answering_optionals(&mut runner, true);
        events.extend(resolved);
        finished(&runner);

        let owner_library = |player: PlayerId| &runner.state().players[player.0 as usize].library;
        if first_group_illegal {
            // The graveyard group still resolves: P0's chosen cards are shuffled
            // into P0's library through the real "you may" prompt.
            assert_eq!(prompts, 1, "the 'you may shuffle' prompt was offered once");
            assert_eq!(distinct_shuffles(&events), vec![P0]);
            for id in [grave_a, grave_b] {
                assert_eq!(runner.state().objects[&id].zone, Zone::Library);
                assert!(owner_library(P0).contains(&id));
            }
            // The illegal artifact is untouched and its owner's (P2's) and
            // controller's (P1's) libraries are not shuffled on its account.
            assert_eq!(runner.state().objects[&artifact].zone, Zone::Exile);
            assert!(!owner_library(P2).contains(&artifact));
            assert!(!owner_library(P1).contains(&artifact));
            assert!(!distinct_shuffles(&events).contains(&P2));
            assert!(!distinct_shuffles(&events).contains(&P1));
        } else {
            // The artifact group still resolves: the OWNER (P2, not the
            // controller P1 and not the caster P0) shuffles it into their library.
            assert_eq!(runner.state().objects[&artifact].zone, Zone::Library);
            assert!(owner_library(P2).contains(&artifact));
            assert!(!owner_library(P1).contains(&artifact));
            assert!(!owner_library(P0).contains(&artifact));
            // The caster's library may or may not be shuffled for the all-illegal
            // "you may shuffle" sentence (CR 701.24c); this row does not pin that
            // separate question, only that the artifact's OWNER is shuffled and the
            // controller (P1) is not.
            let shuffled = distinct_shuffles(&events);
            assert!(shuffled.contains(&P2));
            assert!(!shuffled.contains(&P1));
            // The illegal graveyard cards are untouched.
            for id in [grave_a, grave_b] {
                assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
                assert!(!owner_library(P0).contains(&id));
            }
        }
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    }
}

// CR 400.7 + CR 701.24c: "When this creature dies, shuffle it into its owner's
// library." The Dragon is owned by P1 and controlled by P0. With its death
// trigger on the stack it is exiled from the graveyard, becoming a new object
// (CR 400.7). The trigger cannot find it, yet the owner's library is still
// shuffled (CR 701.24c), and never the controller's.
#[test]
fn alabaster_dragon_exiled_in_response_stays_exiled_but_its_owner_still_shuffles() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dragon = scenario
        .add_creature(P1, "Alabaster Dragon", 4, 4)
        .controlled_by(P0)
        .with_subtypes(vec!["Dragon"])
        .from_oracle_text_with_keywords(&["Flying"], ALABASTER_DRAGON)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{B}{B}"))
        .id();
    scenario.with_library_top(P0, &["P0 Card"]);
    scenario.with_library_top(P1, &["P1 Card"]);
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&dragon].owner, P1);
    assert_eq!(runner.state().objects[&dragon].controller, P0);

    let mut events = runner
        .cast(murder)
        .target_object(dragon)
        .commit()
        .events()
        .to_vec();
    for _ in 0..4 {
        if runner.state().stack.len() == 1
            && runner.state().objects[&murder].zone == Zone::Graveyard
        {
            break;
        }
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }

    // Reach guards: the Dragon died and its death trigger (not Murder) is the
    // only thing on the stack.
    assert!(events.iter().any(|event| matches!(event,
        GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, .. }
        if *object_id == dragon)));
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, dragon);
    assert_eq!(runner.state().objects[&dragon].zone, Zone::Graveyard);

    // In response, the Dragon is exiled from the graveyard (a new object).
    let before = runner.state().objects[&dragon].incarnation;
    engine::game::zones::move_to_zone(runner.state_mut(), dragon, Zone::Exile, &mut vec![]);
    assert_eq!(runner.state().objects[&dragon].zone, Zone::Exile);
    assert_ne!(runner.state().objects[&dragon].incarnation, before);

    let mut resolved = vec![];
    for _ in 0..4 {
        if runner.state().stack.is_empty() {
            break;
        }
        resolved.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    finished(&runner);

    // The trigger could not find the Dragon: it stays in exile, in no library.
    assert_eq!(runner.state().objects[&dragon].zone, Zone::Exile);
    assert!(!runner.state().players[0].library.contains(&dragon));
    assert!(!runner.state().players[1].library.contains(&dragon));
    // CR 701.24c: the owner's library is shuffled anyway, never the controller's.
    assert_eq!(distinct_shuffles(&resolved), vec![P1]);
}

// CR 111.7 + CR 603.10a + CR 701.24c: an Alabaster Dragon TOKEN dies; its death
// trigger still triggers before the token ceases to exist, resolves, finds no
// object to move, and still shuffles the owner's library. The token is the
// printed Dragon marked `is_token` after the scenario is built, not created by
// an effect, so token creation and its owner (CR 111.2) are not exercised here.
#[test]
fn alabaster_dragon_token_death_trigger_resolves_and_still_shuffles_its_owner() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let token = scenario
        .add_creature(P0, "Alabaster Dragon", 4, 4)
        .with_subtypes(vec!["Dragon"])
        .from_oracle_text_with_keywords(&["Flying"], ALABASTER_DRAGON)
        .id();
    let slash = scenario
        .add_spell_to_hand_from_oracle(P0, "Flame Slash", false, FLAME_SLASH)
        .with_mana_cost(parse_mtgjson_mana_cost("{R}"))
        .id();
    scenario.with_library_top(P0, &["P0 Card A", "P0 Card B"]);
    scenario.with_library_top(P1, &["P1 Card"]);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;
    assert_eq!(runner.state().objects[&token].owner, P0);
    let library_before = runner.state().players[0].library.len();

    let mut events = runner
        .cast(slash)
        .target_object(token)
        .commit()
        .events()
        .to_vec();
    for _ in 0..4 {
        if runner.state().stack.len() == 1 && runner.state().objects[&slash].zone == Zone::Graveyard
        {
            break;
        }
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }

    // Reach guards: the token died (battlefield to graveyard) and its death
    // trigger is on the stack before the token ceases to exist.
    assert!(events.iter().any(|event| matches!(event,
        GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Graveyard, .. }
        if *object_id == token)));
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, token);

    let mut resolved = vec![];
    for _ in 0..4 {
        if runner.state().stack.is_empty() {
            break;
        }
        resolved.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    finished(&runner);

    // The token is gone: it is in no library and P0's library did not grow.
    assert!(!runner.state().players[0].library.contains(&token));
    assert!(!runner.state().players[1].library.contains(&token));
    assert_eq!(runner.state().players[0].library.len(), library_before);
    assert!(runner
        .state()
        .objects
        .get(&token)
        .is_none_or(|object| object.zone != Zone::Library));
    // CR 701.24c: the owner's library is still shuffled, and only theirs.
    assert_eq!(distinct_shuffles(&resolved), vec![P0]);
}

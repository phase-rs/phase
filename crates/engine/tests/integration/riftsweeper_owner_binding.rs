//! Ordinary full-Oracle witnesses for R2, independent of Beam casting.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const RIFTSWEEPER: &str = "When this creature enters, choose target face-up exiled card. Its owner shuffles it into their library.";
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

// CR 603.3d + CR 601.2c: with no legal target in exile, the trigger is removed
// from the stack while its targets are being chosen, before it can resolve.
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

    // Two legal face-up cards force a real announcement instead of the
    // unique-target shortcut; selecting P1's card must not shuffle P0's card.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_to_hand(P0, "Riftsweeper", 2, 2)
        .with_subtypes(vec!["Elf", "Shaman"])
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
        .from_oracle_text(RIFTSWEEPER)
        .id();
    let target = scenario.add_land_to_exile(P1, "Island").id();
    let other = scenario.add_land_to_exile(P0, "Forest").id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.cast(source).commit();
    runner.advance_until_stack_empty();
    let WaitingFor::TriggerTargetSelection {
        player,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected exile target announcement: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(target_slots.len(), 1);
    assert_eq!(target_slots[0].legal_targets.len(), 2);
    for candidate in [target, other] {
        assert!(target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(candidate)));
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .unwrap();
    let mut events = Vec::new();
    while !runner.state().stack.is_empty() {
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert_eq!(runner.state().objects[&target].zone, Zone::Library);
    assert!(runner.state().players[1].library.contains(&target));
    assert_eq!(runner.state().objects[&other].zone, Zone::Exile);
    assert_eq!(shuffles(&events), vec![P1]);
    finished(&runner);
    assert!(runner.state().resolution_stack.is_empty());
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

// CR 108.3 + CR 608.2c: the target's owner shuffles and draws, not its controller.
#[test]
fn oblation_owner_draws_two_after_the_shuffle() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .controlled_by(P0)
        .id();
    for _ in 0..20 {
        scenario.add_card_to_library_top(P1, "Island");
        scenario.add_card_to_library_top(P0, "Plains");
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Oblation",
            true,
            "The owner of target nonland permanent shuffles it into their library, then draws two cards.",
        )
        .with_mana_cost(parse_mtgjson_mana_cost("{2}{W}"))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(target).resolve();
    outcome.assert_hand_drawn(P1, 2);
    outcome.assert_hand_drawn(P0, 0);
    outcome.assert_zone(&[target], Zone::Library);
    assert!(outcome.state().players[1].library.contains(&target));
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

// CR 730.3c: a freshly chosen exile target is one card, even when that card is a
// former merged component whose siblings sit beside it in exile. The continuity
// expansion belongs to "return the object it became", not to a new selection.
#[test]
fn riftsweeper_fresh_target_does_not_expand_merged_components() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_to_hand(P0, "Riftsweeper", 2, 2)
        .with_subtypes(vec!["Elf", "Shaman"])
        .with_mana_cost(parse_mtgjson_mana_cost("{1}{G}"))
        .from_oracle_text(RIFTSWEEPER)
        .id();
    let host = scenario.add_creature(P0, "Merged host", 3, 3).id();
    let rider = scenario.add_creature(P0, "Merged rider", 2, 2).id();
    let spare = scenario.add_land_to_exile(P1, "Island").id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    engine::game::merge::merge_object_onto(
        runner.state_mut(),
        rider,
        host,
        engine::game::merge::MergeSide::Bottom,
        &mut Vec::new(),
    );
    runner.state_mut().battlefield.retain(|&id| id != rider);
    // The merged permanent leaves the battlefield: the survivor moves and the
    // absorbed card splits into the same zone with a back-link.
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), host, Zone::Exile, &mut events);
    assert_eq!(runner.state().objects[&host].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&rider].zone, Zone::Exile);
    assert!(runner.state().objects[&rider]
        .split_from_merge_survivor
        .is_some());

    runner.cast(source).commit();
    runner.advance_until_stack_empty();
    let WaitingFor::TriggerTargetSelection { target_slots, .. } = &runner.state().waiting_for
    else {
        panic!(
            "Riftsweeper must announce an exile target: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1);
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Object(host)));
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Object(spare)));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(host)),
        })
        .unwrap();
    let mut events = vec![];
    for _ in 0..2 {
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    finished(&runner);
    assert_eq!(
        shuffles(&events),
        vec![P0],
        "only the chosen card's owner shuffles"
    );
    assert_eq!(runner.state().objects[&host].zone, Zone::Library);
    assert_eq!(
        runner.state().objects[&rider].zone,
        Zone::Exile,
        "the unselected former component stays in exile"
    );
    assert_eq!(runner.state().objects[&spare].zone, Zone::Exile);
}

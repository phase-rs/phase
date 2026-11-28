use std::collections::BTreeSet;

use engine::game::combat::AttackTarget;
use engine::game::mana_sources::{
    activatable_mana_actions_for_player, activatable_mana_source_selections,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{ChosenAttribute, TapCreaturesSelectionMode, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, CostResume, ManaChoice, ManaChoicePrompt, PayCostKind, RandomDraw,
    StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use rand::seq::SliceRandom;

use crate::support::shared_card_db;

fn real(s: &mut GameScenario, p: PlayerId, name: &str, zone: Zone) -> ObjectId {
    s.add_real_card(p, name, zone, shared_card_db().expect("printed fixture"))
}

fn board(s: &mut GameScenario, p: PlayerId, name: &str) -> ObjectId {
    real(s, p, name, Zone::Battlefield)
}

fn table() -> GameScenario {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s
}

fn fund(s: &mut GameScenario, p: PlayerId, units: &[(ManaType, ObjectId)]) {
    s.with_mana_pool(
        p,
        units
            .iter()
            .map(|&(kind, source)| ManaUnit::new(kind, source, false, vec![]))
            .collect(),
    );
}

fn activate(
    r: &mut GameRunner,
    source_id: ObjectId,
    ability_index: usize,
    payment_mode: CastPaymentMode,
) {
    r.act(GameAction::ActivateAbility {
        source_id,
        ability_index,
        payment_mode,
    })
    .unwrap();
}

fn assert_x(r: &GameRunner, source: ObjectId, index: usize, max_x: u32) {
    let WaitingFor::ChooseXValue {
        player,
        min,
        max,
        pending_cast,
        ..
    } = &r.state().waiting_for
    else {
        panic!("expected ChooseXValue, got {:?}", r.state().waiting_for);
    };
    assert_eq!((*player, *min, *max), (P0, 0, max_x));
    assert_eq!(pending_cast.object_id, source);
    assert_eq!(pending_cast.activation_ability_index, Some(index));
    assert_eq!(pending_cast.ability.chosen_x, None);
    assert!(pending_cast.ability.targets.is_empty());
    assert!(!r.state().objects[&source].tapped);
}

fn announce(r: &mut GameRunner, x: u32) {
    r.act(GameAction::ChooseX { value: x }).unwrap();
}

fn tap_choices(
    r: &GameRunner,
    low: usize,
    high: usize,
    mode: TapCreaturesSelectionMode,
) -> &[ObjectId] {
    let WaitingFor::PayCost {
        player,
        kind: PayCostKind::TapCreatures { mode: live_mode },
        choices,
        count,
        min_count,
        ..
    } = &r.state().waiting_for
    else {
        panic!(
            "expected written tap payment, got {:?}",
            r.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!((*min_count, *count, *live_mode), (low, high, mode));
    choices
}

fn exact(r: &GameRunner, n: usize, expected: &[ObjectId]) {
    let choices = tap_choices(r, n, n, TapCreaturesSelectionMode::Fixed);
    assert_eq!(
        choices.iter().copied().collect::<BTreeSet<_>>(),
        expected.iter().copied().collect::<BTreeSet<_>>(),
    );
}

fn targets(r: &GameRunner) -> &[TargetRef] {
    let WaitingFor::TargetSelection {
        player,
        target_slots,
        ..
    } = &r.state().waiting_for
    else {
        panic!("expected TargetSelection, got {:?}", r.state().waiting_for);
    };
    assert_eq!(*player, P0);
    assert_eq!(target_slots.len(), 1);
    &target_slots[0].legal_targets
}

fn select_target(r: &mut GameRunner, target: TargetRef) {
    assert!(targets(r).contains(&target));
    r.act(GameAction::ChooseTarget {
        target: Some(target),
    })
    .unwrap();
}

fn select(r: &mut GameRunner, cards: &[ObjectId]) -> Vec<GameEvent> {
    r.act(GameAction::SelectCards {
        cards: cards.to_vec(),
    })
    .unwrap()
    .events
}

fn fail(r: &mut GameRunner, action: GameAction) {
    let before = payment_state(r);
    assert!(r.act(action).is_err());
    assert_eq!(payment_state(r), before);
}

fn payment_state(r: &GameRunner) -> serde_json::Value {
    let mut state = serde_json::to_value(r.state()).unwrap();
    // Viewing a rejected action can close the previous action's knowledge stamps.
    state
        .as_object_mut()
        .unwrap()
        .remove("zone_change_library_knowledge_stamps");
    state
}

fn tap_rejected(r: &mut GameRunner, cards: &[ObjectId]) {
    fail(
        r,
        GameAction::SelectCards {
            cards: cards.to_vec(),
        },
    );
}

fn on_stack(r: &GameRunner, source: ObjectId, x: u32, depth: usize) {
    assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(r.state().stack.len(), depth);
    let entry = r.state().stack.last().unwrap();
    let StackEntryKind::ActivatedAbility { source_id, ability } = &entry.kind else {
        panic!("expected activated ability on top");
    };
    assert_eq!((*source_id, ability.chosen_x), (source, Some(x)));
}

fn resolve_top(r: &mut GameRunner) -> Vec<GameEvent> {
    let players = r.state().players.len();
    let mut events = Vec::new();
    for _ in 0..players {
        assert!(
            matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
            "{:?}",
            r.state().waiting_for
        );
        events.extend(r.act(GameAction::PassPriority).unwrap().events);
    }
    events
}

fn finished(r: &GameRunner) {
    assert!(r.state().stack.is_empty());
    assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
}

fn subtype_tokens(r: &GameRunner, player: PlayerId, subtype: &str) -> Vec<ObjectId> {
    r.state()
        .objects
        .iter()
        .filter_map(|(&id, object)| {
            (object.is_token
                && object.zone == Zone::Battlefield
                && object.controller == player
                && object
                    .card_types
                    .subtypes
                    .iter()
                    .any(|kind| kind == subtype))
            .then_some(id)
        })
        .collect()
}

#[test]
fn apothecary_white_announces_exact_foods_and_creates_humans() {
    let mut s = table();
    let source = board(&mut s, P0, "Apothecary White");
    let food = board(&mut s, P0, "Gingerbrute");
    let opposing = board(&mut s, P1, "Gingerbrute");
    let decoy = board(&mut s, P0, "Ornithopter");
    let goose = real(&mut s, P0, "Gilded Goose", Zone::Hand);
    fund(
        &mut s,
        P0,
        &[(ManaType::Green, decoy), (ManaType::White, decoy)],
    );
    let mut r = s.build();
    r.cast(goose).resolve();
    let token = subtype_tokens(&r, P0, "Food")[0];
    assert!(!r.state().objects[&food].is_token);
    activate(&mut r, source, 0, CastPaymentMode::Auto);
    assert_x(&r, source, 0, 2);
    announce(&mut r, 2);
    exact(&r, 2, &[food, token]);
    tap_rejected(&mut r, &[food]);
    tap_rejected(&mut r, &[token, opposing]);
    tap_rejected(&mut r, &[token, decoy]);
    select(&mut r, &[food, token]);
    on_stack(&r, source, 2, 1);
    resolve_top(&mut r);
    finished(&r);
    let humans = subtype_tokens(&r, P0, "Human");
    assert_eq!(humans.len(), 2);
    for id in humans {
        let human = &r.state().objects[&id];
        assert_eq!((human.power, human.toughness), (Some(1), Some(1)));
        assert_eq!(human.effective_colors(), [ManaColor::White]);
    }
    assert!(r.state().objects[&food].tapped && r.state().objects[&token].tapped);
    assert!(!r.state().objects[&opposing].tapped && !r.state().objects[&decoy].tapped);
}

#[test]
fn apothecary_white_zero_still_pays_white_and_source_tap() {
    let mut s = table();
    let source = board(&mut s, P0, "Apothecary White");
    fund(&mut s, P0, &[(ManaType::White, source)]);
    let mut r = s.build();
    activate(&mut r, source, 0, CastPaymentMode::Auto);
    assert_x(&r, source, 0, 0);
    announce(&mut r, 0);
    on_stack(&r, source, 0, 1);
    assert!(r.state().objects[&source].tapped);
    assert!(r.state().players[0].mana_pool.is_empty());
    resolve_top(&mut r);
    finished(&r);
    assert!(subtype_tokens(&r, P0, "Human").is_empty());
}

#[test]
fn aryel_locks_power_before_targets_and_rechecks_it_at_resolution() {
    for pump in [false, true] {
        let mut s = table();
        let source = board(&mut s, P0, "Aryel, Knight of Windgrace");
        let first = board(&mut s, P0, "Silver Knight");
        let second = board(&mut s, P0, "White Knight");
        let tapped = board(&mut s, P0, "Black Knight");
        let opposing = board(&mut s, P1, "Knight of Meadowgrain");
        let victim = board(&mut s, P1, "Grizzly Bears");
        let too_big = board(&mut s, P1, "Centaur Courser");
        let strike = real(&mut s, P0, "Defiant Strike", Zone::Hand);
        real(&mut s, P0, "Plains", Zone::Library);
        fund(
            &mut s,
            P0,
            &[(ManaType::Black, source), (ManaType::White, source)],
        );
        let mut r = s.build();
        r.state_mut()
            .objects
            .get_mut(&first)
            .unwrap()
            .summoning_sick = true;
        r.state_mut().objects.get_mut(&tapped).unwrap().tapped = true;
        // CR 302.6: Newly controlled creatures may pay the written tap leg.
        let mut sick_source = GameRunner::from_state(r.state().clone());
        sick_source
            .state_mut()
            .objects
            .get_mut(&source)
            .unwrap()
            .summoning_sick = true;
        fail(
            &mut sick_source,
            GameAction::ActivateAbility {
                source_id: source,
                ability_index: 1,
                payment_mode: CastPaymentMode::Auto,
            },
        );
        activate(&mut r, source, 1, CastPaymentMode::Auto);
        assert_x(&r, source, 1, 2);
        fail(&mut r, GameAction::PassPriority);
        announce(&mut r, 2);
        let legal = targets(&r);
        assert_eq!(legal.len(), 3);
        for id in [first, opposing, victim] {
            assert!(legal.contains(&TargetRef::Object(id)));
        }
        assert!(!targets(&r).contains(&TargetRef::Object(too_big)));
        fail(
            &mut r,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(too_big)),
            },
        );
        select_target(&mut r, TargetRef::Object(victim));
        exact(&r, 2, &[first, second]);
        fail(&mut r, GameAction::PassPriority);
        tap_rejected(&mut r, &[first, opposing]);
        tap_rejected(&mut r, &[first, tapped]);
        select(&mut r, &[first, second]);
        on_stack(&r, source, 2, 1);
        if pump {
            // CR 608.2b: An illegal sole target prevents resolution. X and the
            // paid tap selection belong to the activation already on the stack.
            let _ = r.cast(strike).target_objects(&[victim]).commit();
            resolve_top(&mut r);
            assert_eq!(r.state().objects[&victim].power, Some(3));
            on_stack(&r, source, 2, 1);
        }
        resolve_top(&mut r);
        finished(&r);
        assert!(r.state().objects[&first].tapped && r.state().objects[&second].tapped);
        assert!(r.state().objects[&source].tapped);
        assert_eq!(
            r.state().objects[&victim].zone,
            if pump {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            },
        );
    }
}

fn dig(r: &GameRunner, top: &[ObjectId], selectable: &[ObjectId], optional: bool) {
    let WaitingFor::DigChoice {
        player,
        cards,
        keep_count,
        up_to,
        selectable_cards,
        ..
    } = &r.state().waiting_for
    else {
        panic!("expected DigChoice, got {:?}", r.state().waiting_for);
    };
    assert_eq!((*player, *keep_count, *up_to), (P0, 1, optional));
    assert_eq!(cards, top);
    assert_eq!(selectable_cards, selectable);
}

#[test]
fn belisarius_cawl_limits_optional_artifact_reveal_to_announced_window() {
    for accept in [false, true] {
        let mut s = table();
        let source = board(&mut s, P0, "Belisarius Cawl");
        let first = board(&mut s, P0, "Ornithopter");
        let second = board(&mut s, P0, "Memnite");
        let third = board(&mut s, P0, "Grizzly Bears");
        let wanted = real(&mut s, P0, "Darksteel Ingot", Zone::Library);
        let rest_a = real(&mut s, P0, "Plains", Zone::Library);
        let rest_b = real(&mut s, P0, "Island", Zone::Library);
        let marker = real(&mut s, P0, "Everflowing Chalice", Zone::Library);
        let mut r = s.build();
        activate(&mut r, source, 1, CastPaymentMode::Auto);
        assert_x(&r, source, 1, 3);
        announce(&mut r, 3);
        exact(&r, 3, &[first, second, third]);
        select(&mut r, &[first, second, third]);
        on_stack(&r, source, 3, 1);
        resolve_top(&mut r);
        dig(&r, &[wanted, rest_a, rest_b], &[wanted], true);
        tap_rejected(&mut r, &[marker]);
        // CR 400.5 and CR 608.2c: Only the remainder enters the random-order operation.
        let mut expected_rest = if accept {
            vec![rest_a, rest_b]
        } else {
            vec![wanted, rest_a, rest_b]
        };
        let mut expected_rng = r.state().rng.clone();
        expected_rest.shuffle(expected_rng.draw(RandomDraw::Placement));
        if accept {
            select(&mut r, &[wanted]);
        } else {
            select(&mut r, &[]);
        }
        finished(&r);
        assert_eq!(r.state().rng.get_word_pos(), expected_rng.get_word_pos());
        let after = r.state().players[0]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(
            after,
            std::iter::once(marker)
                .chain(expected_rest)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            r.state().objects[&wanted].zone,
            if accept { Zone::Hand } else { Zone::Library },
        );
        // Selected-artifact-only reveal is still a separate coverage gap.
    }
}

#[test]
fn belisarius_cawl_ultima_founding_keeps_its_printed_fixed_two() {
    let mut s = table();
    let source = board(&mut s, P0, "Belisarius Cawl");
    let first = board(&mut s, P0, "Ornithopter");
    let second = board(&mut s, P0, "Memnite");
    let mut r = s.build();
    activate(&mut r, source, 0, CastPaymentMode::Auto);
    exact(&r, 2, &[first, second]);
    select(&mut r, &[first, second]);
    resolve_top(&mut r);
    finished(&r);
    let tokens = subtype_tokens(&r, P0, "Warrior");
    assert_eq!(tokens.len(), 1);
    assert_eq!(r.state().objects[&tokens[0]].power, Some(2));
    assert!(r.state().objects[&tokens[0]].has_keyword(&Keyword::Vigilance));
}

#[test]
fn glacian_keeps_announced_window_when_source_leaves() {
    for x in [0, 2] {
        let mut s = table();
        let source = board(&mut s, P0, "Glacian, Powerstone Engineer");
        let first = board(&mut s, P0, "Ornithopter");
        let second = board(&mut s, P0, "Memnite");
        let opposing = board(&mut s, P1, "Ornithopter");
        let bounce = real(&mut s, P0, "Unsummon", Zone::Hand);
        let kept = real(&mut s, P0, "Plains", Zone::Library);
        let binned = real(&mut s, P0, "Forest", Zone::Library);
        let untouched = real(&mut s, P0, "Island", Zone::Library);
        fund(&mut s, P0, &[(ManaType::Blue, source)]);
        let mut r = s.build();
        r.state_mut()
            .objects
            .get_mut(&first)
            .unwrap()
            .summoning_sick = true;
        activate(&mut r, source, 0, CastPaymentMode::Auto);
        assert_x(&r, source, 0, 2);
        announce(&mut r, x);
        if x != 0 {
            exact(&r, 2, &[first, second]);
            tap_rejected(&mut r, &[first, opposing]);
            select(&mut r, &[first, second]);
        }
        on_stack(&r, source, x, 1);
        let _ = r.cast(bounce).target_object(source).commit();
        resolve_top(&mut r);
        assert_eq!(r.state().objects[&source].zone, Zone::Hand);
        on_stack(&r, source, x, 1);
        resolve_top(&mut r);
        if x != 0 {
            dig(&r, &[kept, binned], &[kept, binned], false);
            select(&mut r, &[kept]);
            assert_eq!(r.state().objects[&kept].zone, Zone::Hand);
            assert_eq!(r.state().objects[&binned].zone, Zone::Graveyard);
            assert_eq!(r.state().players[0].library.front(), Some(&untouched));
        } else {
            assert_eq!(
                r.state().players[0]
                    .library
                    .iter()
                    .copied()
                    .collect::<Vec<_>>(),
                [kept, binned, untouched]
            );
        }
        finished(&r);
    }
}

fn mana_source(r: &GameRunner, source: ObjectId) -> GameAction {
    if let Some(land_action) = activatable_mana_actions_for_player(r.state(), P0)
        .into_iter()
        .find(|action| {
            matches!(
                action, GameAction::TapLandForMana { selection }
                    if selection.source.object_id == source
            )
        })
    {
        return land_action;
    }
    let selection = activatable_mana_source_selections(r.state(), P0)
        .into_iter()
        .find(|selection| selection.source.object_id == source)
        .unwrap_or_else(|| {
            panic!(
                "no mana action for {source:?} in {:?}",
                r.state().waiting_for
            )
        });
    GameAction::ActivateManaSource { selection }
}

#[test]
fn hazel_announces_exact_tap_count_before_life_taps_or_colors() {
    for x in [0, 2] {
        let mut s = table();
        let source = board(&mut s, P0, "Hazel of the Rootbloom");
        let decoy = board(&mut s, P0, "Ornithopter");
        let goose = real(&mut s, P0, "Gilded Goose", Zone::Hand);
        let chatter = real(&mut s, P0, "Chatter of the Squirrel", Zone::Hand);
        fund(
            &mut s,
            P0,
            &[(ManaType::Green, decoy), (ManaType::Green, decoy)],
        );
        let mut r = s.build();
        r.cast(goose).resolve();
        r.cast(chatter).resolve();
        let food = subtype_tokens(&r, P0, "Food")[0];
        let squirrel = subtype_tokens(&r, P0, "Squirrel")[0];
        let action = mana_source(&r, source);
        r.act(action).unwrap();
        let WaitingFor::PayAmountChoice {
            player,
            resource,
            min,
            max,
            source_id,
            ..
        } = &r.state().waiting_for
        else {
            panic!(
                "expected Hazel amount prompt, got {:?}",
                r.state().waiting_for
            );
        };
        assert_eq!((*player, *source_id, *min, *max), (P0, source, 0, 2));
        assert_eq!(serde_json::to_value(resource).unwrap()["type"], "TapCountX");
        assert_eq!(r.life(P0), 20);
        assert!(r.state().players[0].mana_pool.is_empty() && r.state().stack.is_empty());
        for id in [source, food, squirrel] {
            assert!(!r.state().objects[&id].tapped);
        }
        fail(&mut r, GameAction::SubmitPayAmount { amount: 3 });
        r.act(GameAction::SubmitPayAmount { amount: x }).unwrap();
        if x != 0 {
            exact(&r, 2, &[food, squirrel]);
            tap_rejected(&mut r, &[food]);
            tap_rejected(&mut r, &[food, decoy]);
            select(&mut r, &[food, squirrel]);
            assert!(matches!(
                r.state().waiting_for,
                WaitingFor::ChooseManaColor {
                    player: P0,
                    choice: ManaChoicePrompt::AnyCombination { count: 2, .. },
                    ..
                }
            ));
            r.act(GameAction::ChooseManaColor {
                choice: ManaChoice::Combination(vec![ManaType::White, ManaType::Blue]),
                count: 1,
            })
            .unwrap();
            let pool = &r.state().players[0].mana_pool;
            assert_eq!(pool.count_color(ManaType::White), 1);
            assert_eq!(pool.count_color(ManaType::Blue), 1);
        }
        finished(&r);
        assert_eq!(r.life(P0), 18);
        assert_eq!(r.state().players[0].mana_pool.total(), x as usize);
        assert!(r.state().objects[&source].tapped);
        assert_eq!(r.state().objects[&food].tapped, x == 2);
        assert_eq!(r.state().objects[&squirrel].tapped, x == 2);
        assert!(!r.state().objects[&decoy].tapped);
    }
}

#[test]
fn dockhand_reserves_the_source_and_mana_artifact_during_payment() {
    for manual in [false, true] {
        let mut s = table();
        let source = board(&mut s, P0, "Merchant's Dockhand");
        let mana = board(&mut s, P0, "Mind Stone");
        let helper = board(&mut s, P0, "Ornithopter");
        let opposing = board(&mut s, P1, "Ornithopter");
        let kept = real(&mut s, P0, "Forest", Zone::Library);
        let rest = real(&mut s, P0, "Island", Zone::Library);
        let marker = real(&mut s, P0, "Plains", Zone::Library);
        fund(
            &mut s,
            P0,
            &[
                (ManaType::Blue, source),
                (ManaType::Colorless, source),
                (ManaType::Colorless, source),
            ],
        );
        let mut r = s.build();
        activate(
            &mut r,
            source,
            0,
            if manual {
                CastPaymentMode::Manual
            } else {
                CastPaymentMode::Auto
            },
        );
        // CR 118.3 and CR 601.2h: the source and the only available mana producer
        // each pay their own tap leg. Neither can pay the written tap leg.
        assert_x(&r, source, 0, 1);
        fail(&mut r, GameAction::ChooseX { value: 2 });
        announce(&mut r, 1);
        if manual {
            assert!(matches!(
                r.state().waiting_for,
                WaitingFor::ManaPayment { player: P0, .. }
            ));
            r.act(mana_source(&r, mana)).unwrap();
            r.act(GameAction::PassPriority).unwrap();
        }
        exact(&r, 1, &[helper]);
        for illegal in [source, mana, opposing] {
            tap_rejected(&mut r, &[illegal]);
        }
        tap_rejected(&mut r, &[]);
        tap_rejected(&mut r, &[helper, helper]);
        select(&mut r, &[helper]);
        on_stack(&r, source, 1, 1);
        assert!(r.state().objects[&source].tapped);
        assert!(r.state().objects[&mana].tapped);
        assert!(r.state().objects[&helper].tapped);
        resolve_top(&mut r);
        if matches!(r.state().waiting_for, WaitingFor::DigChoice { .. }) {
            dig(&r, &[kept], &[kept], false);
            select(&mut r, &[kept]);
        }
        finished(&r);
        assert_eq!(r.state().objects[&kept].zone, Zone::Hand);
        assert_eq!(
            r.state().players[0]
                .library
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [rest, marker]
        );
    }
}

#[test]
fn dockhand_at_two_puts_unchosen_card_below_the_untouched_library() {
    let mut s = table();
    let source = board(&mut s, P0, "Merchant's Dockhand");
    let first = board(&mut s, P0, "Ornithopter");
    let second = board(&mut s, P0, "Memnite");
    let kept = real(&mut s, P0, "Forest", Zone::Library);
    let bottom = real(&mut s, P0, "Island", Zone::Library);
    let marker = real(&mut s, P0, "Plains", Zone::Library);
    fund(
        &mut s,
        P0,
        &[
            (ManaType::Blue, source),
            (ManaType::Colorless, source),
            (ManaType::Colorless, source),
            (ManaType::Colorless, source),
        ],
    );
    let mut r = s.build();
    activate(&mut r, source, 0, CastPaymentMode::Auto);
    assert_x(&r, source, 0, 2);
    announce(&mut r, 2);
    exact(&r, 2, &[first, second]);
    select(&mut r, &[first, second]);
    on_stack(&r, source, 2, 1);
    resolve_top(&mut r);
    dig(&r, &[kept, bottom], &[kept, bottom], false);
    select(&mut r, &[kept]);
    finished(&r);
    assert_eq!(r.state().objects[&kept].zone, Zone::Hand);
    assert_eq!(
        r.state().players[0]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        [marker, bottom]
    );
}

#[test]
fn dockhand_zero_reserves_the_only_artifact_as_source_and_leaves_library_alone() {
    let mut s = table();
    let source = board(&mut s, P0, "Merchant's Dockhand");
    let library_top = real(&mut s, P0, "Plains", Zone::Library);
    fund(
        &mut s,
        P0,
        &[
            (ManaType::Blue, source),
            (ManaType::Colorless, source),
            (ManaType::Colorless, source),
            (ManaType::Colorless, source),
        ],
    );
    let mut r = s.build();
    activate(&mut r, source, 0, CastPaymentMode::Auto);
    assert_x(&r, source, 0, 0);
    announce(&mut r, 0);
    on_stack(&r, source, 0, 1);
    assert!(r.state().objects[&source].tapped);
    assert!(r.state().players[0].mana_pool.is_empty());
    resolve_top(&mut r);
    finished(&r);
    assert_eq!(r.state().players[0].library.front(), Some(&library_top));
    assert!(r.state().players[0].hand.is_empty());
}

#[test]
fn necron_overlord_binds_one_x_to_mana_taps_and_each_stack_instance() {
    let p2 = PlayerId(2);
    let mut s = GameScenario::new_n_player(3, 42);
    s.at_phase(Phase::PreCombatMain);
    let source = board(&mut s, P0, "Necron Overlord");
    let first = board(&mut s, P0, "Ornithopter");
    let second = board(&mut s, P0, "Memnite");
    let third = board(&mut s, P0, "Darksteel Citadel");
    let twiddle = real(&mut s, P0, "Twiddle", Zone::Hand);
    fund(
        &mut s,
        P0,
        &[(ManaType::Colorless, first); 3]
            .into_iter()
            .chain([(ManaType::Blue, first)])
            .collect::<Vec<_>>(),
    );
    let mut r = s.build();
    activate(&mut r, source, 0, CastPaymentMode::Auto);
    assert_x(&r, source, 0, 3);
    announce(&mut r, 2);
    assert_eq!(targets(&r), [TargetRef::Player(P1), TargetRef::Player(p2)]);
    select_target(&mut r, TargetRef::Player(P1));
    exact(&r, 2, &[first, second, third]);
    select(&mut r, &[first, second]);
    on_stack(&r, source, 2, 1);
    assert_eq!(r.state().players[0].mana_pool.total(), 2);
    assert!(r.state().objects[&first].tapped && r.state().objects[&second].tapped);
    assert!(!r.state().objects[&third].tapped);

    // Twiddle untaps the already-paid source while the first activation remains on the stack.
    let _ = r.cast(twiddle).target_object(source).commit();
    resolve_top(&mut r);
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));
    r.act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    let WaitingFor::ChooseOneOfBranch { branches, .. } = &r.state().waiting_for else {
        panic!("expected Twiddle's tap-or-untap choice");
    };
    let index = branches
        .iter()
        .position(|branch| branch.description.as_deref() == Some("untap"))
        .unwrap();
    r.act(GameAction::ChooseBranch { index }).unwrap();
    assert!(!r.state().objects[&source].tapped);
    on_stack(&r, source, 2, 1);
    activate(&mut r, source, 0, CastPaymentMode::Auto);
    assert_x(&r, source, 0, 1);
    announce(&mut r, 1);
    select_target(&mut r, TargetRef::Player(p2));
    exact(&r, 1, &[third]);
    select(&mut r, &[third]);
    on_stack(&r, source, 1, 2);
    resolve_top(&mut r);
    assert_eq!((r.life(P1), r.life(p2)), (20, 19));
    on_stack(&r, source, 2, 1);
    resolve_top(&mut r);
    finished(&r);
    assert_eq!((r.life(P0), r.life(P1), r.life(p2)), (20, 18, 19));
    assert!(r.state().players[0].mana_pool.is_empty());
}

#[test]
fn necron_overlord_mana_limited_cap_still_accepts_zero() {
    let mut s = table();
    let source = board(&mut s, P0, "Necron Overlord");
    let first = board(&mut s, P0, "Ornithopter");
    let second = board(&mut s, P0, "Memnite");
    fund(&mut s, P0, &[(ManaType::Colorless, source)]);
    let mut r = s.build();
    activate(&mut r, source, 0, CastPaymentMode::Auto);
    assert_x(&r, source, 0, 1);
    fail(&mut r, GameAction::ChooseX { value: 2 });
    announce(&mut r, 0);
    on_stack(&r, source, 0, 1);
    let StackEntryKind::ActivatedAbility { ability, .. } = &r.state().stack[0].kind else {
        unreachable!("on_stack verified the ability kind");
    };
    assert_eq!(ability.targets, [TargetRef::Player(P1)]);
    resolve_top(&mut r);
    finished(&r);
    assert!(r.state().objects[&source].tapped);
    assert!(!r.state().objects[&first].tapped && !r.state().objects[&second].tapped);
    assert_eq!(r.state().players[0].mana_pool.total(), 1);
    assert_eq!(r.life(P1), 20);
}

#[test]
fn resonance_technician_copies_only_own_selected_value_and_preserves_spell_x() {
    let mut s = table();
    let source = board(&mut s, P0, "Resonance Technician");
    let first = board(&mut s, P0, "Ornithopter");
    let second = board(&mut s, P0, "Memnite");
    let banefire = real(&mut s, P0, "Banefire", Zone::Hand);
    let bolt = real(&mut s, P0, "Lightning Bolt", Zone::Hand);
    let incinerate = real(&mut s, P0, "Incinerate", Zone::Hand);
    let opponent_spell = real(&mut s, P1, "Lightning Strike", Zone::Hand);
    let copy_target = board(&mut s, P1, "Grizzly Bears");
    fund(&mut s, P0, &[(ManaType::Red, source); 5]);
    fund(&mut s, P1, &[(ManaType::Red, source); 2]);
    let mut r = s.build();
    let _ = r.cast(banefire).x(1).target_player(P1).commit();
    let original_entry = r.state().stack.last().unwrap().id;
    let _ = r.cast(bolt).target_player(P1).commit();
    let _ = r.cast(incinerate).target_player(P1).commit();
    let other_own_entry = r.state().stack.last().unwrap().id;
    r.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::Priority { player: P1, .. }
    ));
    let _ = r.cast(opponent_spell).target_player(P0).commit();
    let opposing_entry = r.state().stack.last().unwrap().id;
    r.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::Priority { player: P0, .. }
    ));
    activate(&mut r, source, 0, CastPaymentMode::Auto);
    assert_x(&r, source, 0, 2);
    announce(&mut r, 2);
    let legal = targets(&r);
    assert_eq!(legal.len(), 2);
    assert!(legal.contains(&TargetRef::Object(original_entry)));
    assert!(legal.contains(&TargetRef::Object(other_own_entry)));
    fail(
        &mut r,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(opposing_entry)),
        },
    );
    select_target(&mut r, TargetRef::Object(original_entry));
    exact(&r, 2, &[first, second]);
    select(&mut r, &[first, second]);
    on_stack(&r, source, 2, 5);
    let mut copy_events = resolve_top(&mut r);
    let WaitingFor::CopyRetarget {
        player: P0,
        copy_id,
        ..
    } = r.state().waiting_for
    else {
        panic!(
            "expected the copy retarget prompt, got {:?}",
            r.state().waiting_for
        );
    };
    assert_ne!(copy_id, original_entry);
    let result = r
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(copy_target)),
        })
        .unwrap();
    copy_events.extend(result.events);
    assert!(!copy_events
        .iter()
        .any(|event| matches!(event, GameEvent::SpellCast { .. })));
    let StackEntryKind::Spell {
        ability: Some(copy),
        ..
    } = &r.state().stack.last().unwrap().kind
    else {
        panic!("expected the spell copy");
    };
    assert_eq!(copy.chosen_x, Some(1));
    assert_eq!(copy.targets, [TargetRef::Object(copy_target)]);
    resolve_top(&mut r);
    assert_eq!(r.state().objects[&copy_target].damage_marked, 1);
    assert_eq!(r.life(P1), 20);
    assert_eq!(r.state().stack.len(), 4);
}

#[test]
fn secluded_starforge_fixed_taps_pump_and_respect_sorcery_timing() {
    let mut s = table();
    let source = board(&mut s, P0, "Secluded Starforge");
    let first = board(&mut s, P0, "Ornithopter");
    let second = board(&mut s, P0, "Memnite");
    let creature = board(&mut s, P0, "Grizzly Bears");
    let growth = real(&mut s, P0, "Giant Growth", Zone::Hand);
    real(&mut s, P1, "Forest", Zone::Library);
    fund(&mut s, P0, &[(ManaType::Green, source); 3]);
    let mut r = s.build();
    let request = GameAction::ActivateAbility {
        source_id: source,
        ability_index: 1,
        payment_mode: CastPaymentMode::Auto,
    };
    let mut during_combat = GameRunner::from_state(r.state().clone());
    during_combat.advance_to_combat();
    fail(&mut during_combat, request.clone());
    let _ = r.cast(growth).target_object(creature).commit();
    fail(&mut r, request.clone());
    resolve_top(&mut r);
    r.act(request).unwrap();
    assert_x(&r, source, 1, 2);
    announce(&mut r, 2);
    select_target(&mut r, TargetRef::Object(creature));
    exact(&r, 2, &[first, second]);
    select(&mut r, &[first, second]);
    on_stack(&r, source, 2, 1);
    resolve_top(&mut r);
    finished(&r);
    assert_eq!(
        (
            r.state().objects[&creature].power,
            r.state().objects[&creature].toughness
        ),
        (Some(7), Some(5))
    );
    r.advance_to_combat();
    r.declare_attackers(&[]).unwrap();
    r.advance_to_phase(Phase::Upkeep);
    assert_eq!(r.state().phase, Phase::Upkeep);
    assert_eq!(
        (
            r.state().objects[&creature].power,
            r.state().objects[&creature].toughness
        ),
        (Some(2), Some(2))
    );
}

#[test]
fn secluded_starforge_large_board_keeps_all_forty_helpers_offered() {
    let mut s = table();
    let source = board(&mut s, P0, "Secluded Starforge");
    let creature = board(&mut s, P0, "Grizzly Bears");
    let helpers = (0..40)
        .map(|_| board(&mut s, P0, "Ornithopter"))
        .collect::<Vec<_>>();
    fund(&mut s, P0, &[(ManaType::Colorless, source); 2]);
    let mut r = s.build();
    activate(&mut r, source, 1, CastPaymentMode::Auto);
    assert_x(&r, source, 1, 40);
    announce(&mut r, 40);
    select_target(&mut r, TargetRef::Object(creature));
    exact(&r, 40, &helpers);
    select(&mut r, &helpers);
    on_stack(&r, source, 40, 1);
    resolve_top(&mut r);
    finished(&r);
    assert_eq!(r.state().objects[&creature].power, Some(42));
    assert!(helpers.iter().all(|id| r.state().objects[id].tapped));
}

fn attack_for_tap_x(r: &mut GameRunner, source: ObjectId, accept: bool) {
    r.advance_to_combat();
    r.declare_attackers(&[(source, AttackTarget::Player(P1))])
        .unwrap();
    assert_eq!(r.state().stack.len(), 1);
    let StackEntryKind::TriggeredAbility {
        source_id, ability, ..
    } = &r.state().stack[0].kind
    else {
        panic!("expected an attack trigger");
    };
    assert_eq!(*source_id, source);
    assert!(ability.targets.is_empty());
    resolve_top(r);
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));
    r.act(GameAction::DecideOptionalEffect { accept }).unwrap();
}

#[test]
fn myr_battlesphere_resolves_three_zero_or_declined_myr_taps() {
    for (accept, n) in [(true, 3), (true, 0), (false, 0)] {
        let mut s = table();
        let source = board(&mut s, P0, "Myr Battlesphere");
        let myr = (0..4)
            .map(|_| board(&mut s, P0, "Myr Retriever"))
            .collect::<Vec<_>>();
        let mut r = s.build();
        if n == 0 {
            r.state_mut().objects.get_mut(&source).unwrap().cost_x_paid = Some(5);
        }
        attack_for_tap_x(&mut r, source, accept);
        if accept {
            let choices = tap_choices(&r, 0, 4, TapCreaturesSelectionMode::VariableX);
            assert_eq!(
                choices.iter().copied().collect::<BTreeSet<_>>(),
                myr.iter().copied().collect()
            );
            assert!(matches!(
                r.state().waiting_for,
                WaitingFor::PayCost {
                    resume: CostResume::Resolution,
                    ..
                }
            ));
            let events = select(&mut r, &myr[..n]);
            let damage: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::DamageDealt {
                        source_id,
                        target,
                        amount,
                        ..
                    } if *source_id == source => Some((target.clone(), *amount)),
                    _ => None,
                })
                .collect();
            if n == 0 {
                assert!(damage.is_empty());
            } else {
                assert_eq!(damage, [(TargetRef::Player(P1), n as u32)]);
            }
        }
        finished(&r);
        assert_eq!(r.state().objects[&source].power, Some(4 + n as i32));
        assert_eq!(r.life(P1), 20 - n as i32);
        for (index, id) in myr.iter().enumerate() {
            assert_eq!(r.state().objects[id].tapped, index < n);
        }
    }
}

#[test]
fn myr_battlesphere_damages_attacked_planeswalker_after_the_source_leaves() {
    let mut s = table();
    let source = board(&mut s, P0, "Myr Battlesphere");
    let helper = board(&mut s, P0, "Myr Retriever");
    let spare = board(&mut s, P0, "Myr Retriever");
    let target = board(&mut s, P1, "Karn, Scion of Urza");
    let bounce = real(&mut s, P0, "Unsummon", Zone::Hand);
    let island = board(&mut s, P0, "Island");
    let mut r = s.build();
    r.advance_to_combat();
    r.declare_attackers(&[(source, AttackTarget::Planeswalker(target))])
        .unwrap();
    assert_eq!(r.state().stack.len(), 1);
    assert_eq!(r.state().stack[0].source_id, source);
    r.act(mana_source(&r, island)).unwrap();
    let _ = r.cast(bounce).target_object(source).commit();
    resolve_top(&mut r);
    assert_eq!(r.state().objects[&source].zone, Zone::Hand);
    assert_eq!(r.state().stack.len(), 1);
    resolve_top(&mut r);
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));
    r.act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    let choices = tap_choices(&r, 0, 2, TapCreaturesSelectionMode::VariableX);
    assert_eq!(
        choices.iter().copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([helper, spare])
    );
    select(&mut r, &[helper]);
    finished(&r);
    assert_eq!(r.state().objects[&target].loyalty, Some(4));
    assert_eq!(r.state().objects[&source].zone, Zone::Hand);
    assert_eq!(r.life(P1), 20);
}

#[test]
fn battle_myrsphere_preserves_resolution_time_underfill_and_zero() {
    for n in [0, 1] {
        let mut s = table();
        // The opposing Siege is placed first: scenario setup hands priority to
        // the controller of the last battlefield entry it finishes.
        let opposing = board(&mut s, P1, "Invasion of Zendikar");
        let source = board(&mut s, P0, "Battle Myrsphere");
        let first = board(&mut s, P0, "Invasion of Zendikar");
        let second = board(&mut s, P0, "Invasion of Ikoria");
        let tapped = board(&mut s, P0, "Invasion of Shandalar");
        let mut r = s.build();
        r.state_mut().objects.get_mut(&tapped).unwrap().tapped = true;
        // CR 310.9b: These pre-existing Sieges already have their chosen protectors.
        for (battle, protector) in [(first, P1), (second, P1), (tapped, P1), (opposing, P0)] {
            r.state_mut()
                .objects
                .get_mut(&battle)
                .unwrap()
                .chosen_attributes
                .push(ChosenAttribute::Player(protector));
        }
        let base = r.state().objects[&source].power.unwrap();
        r.advance_to_combat();
        r.declare_attackers(&[(source, AttackTarget::Player(P1))])
            .unwrap();
        // The face's separate damage-recipient defect asks for a target when
        // the trigger is put on the stack. Answer it so the tap payment the
        // trigger offers at resolution can be measured.
        assert!(matches!(
            r.state().waiting_for,
            WaitingFor::TriggerTargetSelection { player: P0, .. }
        ));
        r.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(opposing)),
        })
        .unwrap();
        resolve_top(&mut r);
        assert!(matches!(
            r.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ));
        r.act(GameAction::DecideOptionalEffect { accept: true })
            .unwrap();
        let choices = tap_choices(&r, 0, 2, TapCreaturesSelectionMode::VariableX);
        assert_eq!(
            choices.iter().copied().collect::<BTreeSet<_>>(),
            BTreeSet::from([first, second])
        );
        tap_rejected(&mut r, &[tapped]);
        tap_rejected(&mut r, &[opposing]);
        select(&mut r, &[first][..n]);
        finished(&r);
        assert_eq!(r.state().objects[&source].power, Some(base + n as i32));
        assert_eq!(r.state().objects[&first].tapped, n == 1);
        assert!(!r.state().objects[&second].tapped);
        // The recipient defect is outside this tap-payment release.
    }
}

#[test]
fn secluded_starforge_mana_and_robot_siblings_keep_their_printed_costs() {
    for mana in [true, false] {
        let mut s = table();
        let source = board(&mut s, P0, "Secluded Starforge");
        if !mana {
            fund(&mut s, P0, &[(ManaType::Colorless, source); 5]);
        }
        let mut r = s.build();
        if mana {
            r.act(mana_source(&r, source)).unwrap();
            assert_eq!(
                r.state().players[0]
                    .mana_pool
                    .count_color(ManaType::Colorless),
                1
            );
        } else {
            activate(&mut r, source, 2, CastPaymentMode::Auto);
            assert_eq!(r.state().stack.len(), 1);
            resolve_top(&mut r);
            assert_eq!(subtype_tokens(&r, P0, "Robot").len(), 1);
            assert!(r.state().players[0].mana_pool.is_empty());
        }
        assert!(r.state().objects[&source].tapped);
        finished(&r);
    }
}

#[test]
fn hazel_resumes_a_parent_manual_mana_payment() {
    let mut s = table();
    let source = board(&mut s, P0, "Hazel of the Rootbloom");
    let land = board(&mut s, P0, "Forest");
    let goose = real(&mut s, P0, "Gilded Goose", Zone::Hand);
    let chatter = real(&mut s, P0, "Chatter of the Squirrel", Zone::Hand);
    let parent = real(&mut s, P0, "Grizzly Bears", Zone::Hand);
    fund(&mut s, P0, &[(ManaType::Green, land); 2]);
    let mut r = s.build();
    r.cast(goose).resolve();
    r.cast(chatter).resolve();
    let food = subtype_tokens(&r, P0, "Food")[0];
    let squirrel = subtype_tokens(&r, P0, "Squirrel")[0];
    let card_id = r.state().objects[&parent].card_id;
    r.act(GameAction::CastSpell {
        object_id: parent,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Manual,
    })
    .unwrap();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    r.act(mana_source(&r, source)).unwrap();
    assert!(
        matches!(r.state().waiting_for, WaitingFor::PayAmountChoice { player: P0, source_id, max: 2, .. } if source_id == source)
    );
    assert_eq!(r.life(P0), 20);
    r.act(GameAction::SubmitPayAmount { amount: 2 }).unwrap();
    exact(&r, 2, &[food, squirrel]);
    select(&mut r, &[food, squirrel]);
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::ChooseManaColor {
            player: P0,
            choice: ManaChoicePrompt::AnyCombination { count: 2, .. },
            ..
        }
    ));
    r.act(GameAction::ChooseManaColor {
        choice: ManaChoice::Combination(vec![ManaType::Green, ManaType::Blue]),
        count: 1,
    })
    .unwrap();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert_eq!(r.state().pending_cast.as_ref().unwrap().object_id, parent);
    // CR 605.3a: Hazel's mana ability uses no stack entry. The only entry is
    // the parent spell, which is on the stack while its costs are paid.
    assert_eq!(r.state().stack.len(), 1);
    assert!(matches!(
        &r.state().stack[0].kind,
        StackEntryKind::Spell { card_id: spell, .. } if *spell == card_id
    ));
    assert_eq!(r.life(P0), 18);
    for id in [source, food, squirrel] {
        assert!(r.state().objects[&id].tapped);
    }
    r.act(GameAction::PassPriority).unwrap();
    assert_eq!(r.state().stack.len(), 1);
    resolve_top(&mut r);
    finished(&r);
    assert_eq!(r.state().objects[&parent].zone, Zone::Battlefield);
    assert!(r.state().players[0].mana_pool.is_empty());
}

#[test]
fn hazel_cannot_offer_an_amount_without_source_tap_and_life() {
    for tapped in [false, true] {
        let mut s = table();
        let source = board(&mut s, P0, "Hazel of the Rootbloom");
        s.with_life(P0, if tapped { 20 } else { 1 });
        let mut r = s.build();
        r.state_mut().objects.get_mut(&source).unwrap().tapped = tapped;
        assert!(!activatable_mana_source_selections(r.state(), P0)
            .iter()
            .any(|selection| selection.source.object_id == source));
        finished(&r);
    }
}

fn offers_activation(r: &GameRunner, source: ObjectId, index: usize) -> bool {
    engine::ai_support::legal_actions(r.state())
        .iter()
        .any(|action| {
            matches!(
                action,
                GameAction::ActivateAbility { source_id, ability_index, .. }
                    if *source_id == source && *ability_index == index
            )
        })
}

#[test]
fn aryel_without_a_black_source_is_not_offered_even_with_a_land_in_hand() {
    for land_in_hand in [false, true] {
        let mut s = table();
        let source = board(&mut s, P0, "Aryel, Knight of Windgrace");
        board(&mut s, P0, "Forest");
        board(&mut s, P0, "Silver Knight");
        board(&mut s, P1, "Ornithopter");
        if land_in_hand {
            real(&mut s, P0, "Forest", Zone::Hand);
        }
        let r = s.build();
        // CR 601.2h + CR 118.3: "{B}, {T}, Tap X untapped Knights you
        // control" can't be paid without black mana, whatever X is.
        assert!(!offers_activation(&r, source, 1));
    }
}

#[test]
fn necron_overlord_cap_stays_mana_bounded_with_a_land_in_hand() {
    let mut s = table();
    let source = board(&mut s, P0, "Necron Overlord");
    for name in ["Ornithopter", "Ornithopter", "Memnite", "Memnite"] {
        board(&mut s, P0, name);
    }
    board(&mut s, P0, "Plains");
    real(&mut s, P0, "Forest", Zone::Hand);
    let mut r = s.build();
    activate(&mut r, source, 0, CastPaymentMode::Auto);
    // CR 107.3a + CR 601.2h: one Plains pays {X} only for X = 1, even though
    // four artifacts could be tapped and a land card sits in hand.
    assert_x(&r, source, 0, 1);
    fail(&mut r, GameAction::ChooseX { value: 2 });
}

#[test]
fn apothecary_white_cap_counts_only_untapped_foods() {
    let mut s = table();
    let source = board(&mut s, P0, "Apothecary White");
    let untapped = board(&mut s, P0, "Gingerbrute");
    let tapped = board(&mut s, P0, "Gingerbrute");
    board(&mut s, P0, "Plains");
    let mut r = s.build();
    r.state_mut().objects.get_mut(&tapped).unwrap().tapped = true;
    activate(&mut r, source, 0, CastPaymentMode::Auto);
    // CR 118.3: a tapped Food can't pay "Tap X untapped Foods", and nothing on
    // this board can untap it during the mana window.
    assert_x(&r, source, 0, 1);
    fail(&mut r, GameAction::ChooseX { value: 2 });
    announce(&mut r, 1);
    exact(&r, 1, &[untapped]);
}

#[test]
fn secluded_starforge_cap_counts_a_tapped_pili_pala_that_can_untap_for_mana() {
    let mut s = table();
    let source = board(&mut s, P0, "Secluded Starforge");
    let pili_pala = board(&mut s, P0, "Pili-Pala");
    board(&mut s, P0, "Grizzly Bears");
    fund(&mut s, P0, &[(ManaType::Colorless, source); 4]);
    let mut r = s.build();
    r.state_mut().objects.get_mut(&pili_pala).unwrap().tapped = true;
    activate(&mut r, source, 1, CastPaymentMode::Auto);
    // CR 601.2g + CR 107.6: Pili-Pala's "{2}, {Q}: Add one mana of any color"
    // can untap it during the mana window, so it can still pay for X = 1.
    assert_x(&r, source, 1, 1);
}

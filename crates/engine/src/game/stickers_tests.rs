use crate::game::effects;
use crate::game::layers::evaluate_layers;
use crate::game::scenario::{GameScenario, P0, P1};
use crate::game::stickers::{
    apply_selected_sticker, available_sticker_candidates, set_player_sticker_sheets,
};
use crate::game::zones::move_to_zone;
use crate::types::ability::{
    Effect, QuantityExpr, ResolvedAbility, StickerTicketCostPayment, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::WaitingFor;
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaCost, ManaType, ManaUnit};
use crate::types::phase::Phase;
use crate::types::player::PlayerCounterKind;
use crate::types::stickers::{AppliedSticker, StickerKind};
use crate::types::zones::Zone;
use crate::types::{GameAction, ObjectId};

#[test]
fn stickers_modify_battlefield_object_and_public_zone_retention() {
    let mut scenario = GameScenario::new();
    let creature_id = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut game = scenario.build();
    let state = game.state_mut();

    set_player_sticker_sheets(
        state,
        P0,
        &[
            "Ancestral Hot Dog Minotaur".to_string(),
            "Playable Delusionary Hydra".to_string(),
        ],
    );
    state.players[0].add_player_counters(&PlayerCounterKind::Ticket, 20);

    let mut name = available_sticker_candidates(state, P0, Some(StickerKind::Name), None, false)
        .into_iter()
        .find(|candidate| {
            matches!(
                &candidate.sticker,
                AppliedSticker::Name { text, .. } if text == "Hot Dog"
            )
        })
        .expect("hot dog sticker available");
    if let AppliedSticker::Name { position, .. } = &mut name.sticker {
        *position = 1;
    }

    let flying = available_sticker_candidates(state, P0, Some(StickerKind::Ability), None, false)
        .into_iter()
        .find(|candidate| {
            matches!(
                &candidate.sticker,
                AppliedSticker::Ability { text, .. } if text == "Flying"
            )
        })
        .expect("flying sticker available");

    let pt =
        available_sticker_candidates(state, P0, Some(StickerKind::PowerToughness), Some(5), false)
            .into_iter()
            .find(|candidate| {
                matches!(
                    &candidate.sticker,
                    AppliedSticker::PowerToughness {
                        power: 8,
                        toughness: 6,
                        ..
                    }
                )
            })
            .expect("8/6 sticker available");

    let mut events = Vec::new();
    apply_selected_sticker(
        state,
        P0,
        creature_id,
        name.sticker,
        name.pay_ticket,
        &mut events,
    );
    apply_selected_sticker(
        state,
        P0,
        creature_id,
        flying.sticker,
        flying.pay_ticket,
        &mut events,
    );
    apply_selected_sticker(
        state,
        P0,
        creature_id,
        pt.sticker,
        pt.pay_ticket,
        &mut events,
    );
    evaluate_layers(state);

    let creature = state.objects.get(&creature_id).unwrap();
    assert_eq!(creature.name, "Bear Hot Dog");
    assert_eq!(creature.power, Some(8));
    assert_eq!(creature.toughness, Some(6));
    assert!(creature.has_keyword(&Keyword::Flying));
    assert_eq!(creature.stickers.len(), 3);

    move_to_zone(state, creature_id, Zone::Graveyard, &mut events);
    let graveyard_creature = state.objects.get(&creature_id).unwrap();
    assert_eq!(graveyard_creature.zone, Zone::Graveyard);
    assert_eq!(graveyard_creature.name, "Bear Hot Dog");
    assert_eq!(graveyard_creature.power, Some(8));
    assert_eq!(graveyard_creature.toughness, Some(6));
    assert!(graveyard_creature.has_keyword(&Keyword::Flying));
    assert_eq!(graveyard_creature.stickers.len(), 3);

    move_to_zone(state, creature_id, Zone::Battlefield, &mut events);
    let returned_creature = state.objects.get(&creature_id).unwrap();
    assert_eq!(returned_creature.zone, Zone::Battlefield);
    assert_eq!(returned_creature.name, "Bear Hot Dog");
    assert_eq!(returned_creature.power, Some(8));
    assert_eq!(returned_creature.toughness, Some(6));
    assert!(returned_creature.has_keyword(&Keyword::Flying));
    assert_eq!(returned_creature.stickers.len(), 3);

    move_to_zone(state, creature_id, Zone::Hand, &mut events);
    let hand_creature = state.objects.get(&creature_id).unwrap();
    assert_eq!(hand_creature.zone, Zone::Hand);
    assert_eq!(hand_creature.name, "Bear");
    assert_eq!(hand_creature.power, Some(2));
    assert_eq!(hand_creature.toughness, Some(2));
    assert!(!hand_creature.has_keyword(&Keyword::Flying));
    assert!(hand_creature.stickers.is_empty());
}

#[test]
fn put_sticker_effect_auto_applies_single_eligible_choice() {
    let mut scenario = GameScenario::new();
    let creature_id = scenario.add_creature(P0, "Turtle", 2, 2).id();
    let mut game = scenario.build();
    let state = game.state_mut();

    set_player_sticker_sheets(state, P0, &["Playable Delusionary Hydra".to_string()]);

    let ability = ResolvedAbility::new(
        Effect::PutSticker {
            target: TargetFilter::SpecificObject { id: creature_id },
            kind: Some(StickerKind::PowerToughness),
            count: QuantityExpr::Fixed { value: 1 },
            max_ticket_cost: Some(QuantityExpr::Fixed { value: 2 }),
            ticket_cost_payment: StickerTicketCostPayment::WithoutPaying,
        },
        Vec::new(),
        creature_id,
        P0,
    );
    let mut events = Vec::<GameEvent>::new();
    effects::stickers::resolve(state, &ability, &mut events).unwrap();
    evaluate_layers(state);

    let creature = state.objects.get(&creature_id).unwrap();
    assert_eq!(creature.power, Some(1));
    assert_eq!(creature.toughness, Some(5));
    assert_eq!(creature.stickers.len(), 1);
}

#[test]
fn cast_up_to_two_name_stickers_resolves_via_quantity_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Draw A", "P0 Draw B"]);
    scenario.with_library_top(P1, &["P1 Draw A", "P1 Draw B"]);
    let creature_id = scenario.add_creature(P0, "Bear", 2, 2).id();
    let spell_id = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Two Stickers",
            false,
            "Put up to two name stickers on target creature you own.",
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();
    set_player_sticker_sheets(
        runner.state_mut(),
        P0,
        &["Ancestral Hot Dog Minotaur".to_string()],
    );

    let _commit = runner.cast(spell_id).target_object(creature_id).commit();
    runner.pass_both_players();

    let mut chose_count_branch = false;
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::ChooseOneOfBranch {
                branch_descriptions,
                ..
            } => {
                if let Some(index) = branch_descriptions
                    .iter()
                    .position(|description| description.contains("Put 2 stickers"))
                {
                    runner
                        .act(GameAction::ChooseBranch { index })
                        .expect("choose the two-sticker branch");
                    chose_count_branch = true;
                } else {
                    runner
                        .act(GameAction::ChooseBranch { index: 0 })
                        .expect("choose first sticker option");
                }
            }
            WaitingFor::Priority { .. } => {
                if chose_count_branch
                    && runner.state().stack.is_empty()
                    && runner.state().deferred_triggers.is_empty()
                {
                    break;
                }
                runner.pass_both_players();
            }
            other => panic!("unexpected waiting state while resolving stickers: {other:?}"),
        }
    }

    evaluate_layers(runner.state_mut());
    let creature = runner.state().objects.get(&creature_id).unwrap();
    assert_eq!(creature.zone, Zone::Battlefield);
    assert_eq!(creature.stickers.len(), 2);
}

#[test]
fn cast_up_to_one_name_sticker_allows_choosing_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["P0 Draw A", "P0 Draw B"]);
    scenario.with_library_top(P1, &["P1 Draw A", "P1 Draw B"]);
    let creature_id = scenario.add_creature(P0, "Bear", 2, 2).id();
    let spell_id = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "One Sticker Maybe",
            false,
            "Put up to one name sticker on target creature you own.",
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();
    set_player_sticker_sheets(
        runner.state_mut(),
        P0,
        &["Ancestral Hot Dog Minotaur".to_string()],
    );

    let _commit = runner.cast(spell_id).target_object(creature_id).commit();
    runner.pass_both_players();

    match &runner.state().waiting_for {
        WaitingFor::ChooseOneOfBranch {
            branch_descriptions,
            ..
        } => {
            assert!(
                branch_descriptions
                    .iter()
                    .any(|description| description.contains("Do not put a sticker")),
                "expected zero-choice branch, got {:?}",
                branch_descriptions
            );
        }
        other => panic!("expected count-choice prompt, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseBranch { index: 0 })
        .expect("choose zero stickers");

    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && runner.state().deferred_triggers.is_empty() {
                    break;
                }
                runner.pass_both_players();
            }
            other => panic!("unexpected waiting state after choosing zero: {other:?}"),
        }
    }

    evaluate_layers(runner.state_mut());
    let creature = runner.state().objects.get(&creature_id).unwrap();
    assert_eq!(creature.zone, Zone::Battlefield);
    assert!(creature.stickers.is_empty());
    assert_eq!(creature.name, "Bear");
}

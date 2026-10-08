use crate::game::effects;
use crate::game::layers::evaluate_layers;
use crate::game::scenario::{GameScenario, P0, P1};
use crate::game::stickers::{
    apply_selected_sticker, available_sticker_candidates, set_player_sticker_sheets,
};
use crate::game::zones::move_to_zone;
use crate::types::ability::{
    Effect, QuantityExpr, ResolvedAbility, StickerTicketCostPayment, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectIncarnationRef;
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaCost, ManaType, ManaUnit};
use crate::types::phase::Phase;
use crate::types::player::{PlayerCounterKind, PlayerId};
use crate::types::stickers::{AppliedSticker, StickerKind, StickerLocator};
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

/// The `PutSticker` shape an enters trigger's "put a … sticker on it" carries
/// at resolution: `ParentTarget`, with the entered object (if any) seeded into
/// `targets` at stack push (CR 603.6).
fn put_sticker(
    kind: StickerKind,
    count: QuantityExpr,
    targets: Vec<TargetRef>,
    source: ObjectId,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::PutSticker {
            target: TargetFilter::ParentTarget,
            kind: Some(kind),
            count,
            max_ticket_cost: Some(QuantityExpr::Fixed { value: 2 }),
            ticket_cost_payment: StickerTicketCostPayment::WithoutPaying,
        },
        targets,
        source,
        P0,
    )
}

fn fixed(value: i32) -> QuantityExpr {
    QuantityExpr::Fixed { value }
}

fn up_to(value: i32) -> QuantityExpr {
    QuantityExpr::UpTo {
        max: Box::new(fixed(value)),
    }
}

/// How the resolution's "that sticker" record relates to `object`'s stickers:
/// `"placed"` when it is exactly the (single) sticker on the object, `"none"`
/// when there is no record, `"other"` otherwise.
fn that_sticker(state: &GameState, object: ObjectId) -> &'static str {
    match (
        &state.placed_sticker_this_resolution,
        state.objects[&object].stickers.as_slice(),
    ) {
        (None, _) => "none",
        (Some(record), [placed]) if record == placed => "placed",
        (Some(_), _) => "other",
    }
}

/// CR 123.3b: a player can't put a sticker on an object they don't own; that
/// part of the effect does nothing. (a) At the PutSticker resolver: no sticker
/// on an object controlled by the placing player but owned by another. (b) At
/// the placement authority: no sticker and no ticket paid (CR 123.3c: the
/// owner pays). Every entry is built from a fresh state, and all entries are
/// collected before one table assertion so a failure reports every half.
#[test]
fn put_sticker_refuses_objects_the_placing_player_does_not_own() {
    let mut observed: Vec<(&'static str, PlayerId, usize, u32, &'static str)> = Vec::new();

    // (a) Resolver: the seeded `ParentTarget` object is controlled by P0 (the
    // ability's controller) and owned by `owner`.
    for owner in [P0, P1] {
        let mut scenario = GameScenario::new();
        let object = scenario.add_creature(P0, "Turtle", 2, 2).id();
        let mut game = scenario.build();
        let state = game.state_mut();
        state.objects.get_mut(&object).unwrap().owner = owner;
        set_player_sticker_sheets(state, P0, &["Playable Delusionary Hydra".to_string()]);
        let mut events = Vec::<GameEvent>::new();
        effects::stickers::resolve(
            state,
            &put_sticker(
                StickerKind::PowerToughness,
                fixed(1),
                vec![TargetRef::Object(object)],
                object,
            ),
            &mut events,
        )
        .unwrap();
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        observed.push((
            "resolver",
            owner,
            state.objects[&object].stickers.len(),
            state.players[0].player_counter(&PlayerCounterKind::Ticket),
            that_sticker(state, object),
        ));
    }

    // (b) Placement authority: a paid P/T sticker (ticket cost 2) on an object
    // P0 controls, owned by `owner`.
    for owner in [P0, P1] {
        let mut scenario = GameScenario::new();
        let object = scenario.add_creature(P0, "Turtle", 2, 2).id();
        let mut game = scenario.build();
        let state = game.state_mut();
        state.objects.get_mut(&object).unwrap().owner = owner;
        set_player_sticker_sheets(state, P0, &["Playable Delusionary Hydra".to_string()]);
        state.players[0].add_player_counters(&PlayerCounterKind::Ticket, 5);
        let candidate = available_sticker_candidates(
            state,
            P0,
            Some(StickerKind::PowerToughness),
            Some(2),
            false,
        )
        .into_iter()
        .next()
        .expect("the cost-2 P/T sticker is affordable");
        assert!(candidate.pay_ticket);
        let mut events = Vec::<GameEvent>::new();
        apply_selected_sticker(
            state,
            P0,
            object,
            candidate.sticker,
            candidate.pay_ticket,
            &mut events,
        );
        observed.push((
            "authority",
            owner,
            state.objects[&object].stickers.len(),
            state.players[0].player_counter(&PlayerCounterKind::Ticket),
            that_sticker(state, object),
        ));
    }

    // Owner == placing player: placed (and, at the authority, paid 2 of 5),
    // and that placed sticker is "that sticker" (CR 608.2c).
    // CR 123.3b: owner != placing player: nothing placed, nothing paid, and
    // no "that sticker".
    assert_eq!(
        observed,
        vec![
            ("resolver", P0, 1, 0, "placed"),
            ("resolver", P1, 0, 0, "none"),
            ("authority", P0, 1, 3, "placed"),
            ("authority", P1, 0, 5, "none"),
        ]
    );
}

/// CR 123.3b: a name-sticker PutSticker on an object the placing player
/// doesn't own offers no sticker choice at all (every outcome would do
/// nothing), while the owned object gets the several-candidate prompt.
#[test]
fn put_sticker_offers_no_choice_for_an_object_the_player_does_not_own() {
    for owner in [P0, P1] {
        let mut scenario = GameScenario::new();
        let object = scenario.add_creature(P0, "Turtle", 2, 2).id();
        let mut game = scenario.build();
        let state = game.state_mut();
        state.objects.get_mut(&object).unwrap().owner = owner;
        set_player_sticker_sheets(state, P0, &["Ancestral Hot Dog Minotaur".to_string()]);
        let mut events = Vec::<GameEvent>::new();
        effects::stickers::resolve(
            state,
            &put_sticker(
                StickerKind::Name,
                fixed(1),
                vec![TargetRef::Object(object)],
                object,
            ),
            &mut events,
        )
        .unwrap();
        if owner == P0 {
            // Reach-guard: the owned object is offered the name-sticker choice.
            assert!(
                matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
                "owner P0 must be offered a sticker choice: {:?}",
                state.waiting_for
            );
        } else {
            assert!(
                matches!(state.waiting_for, WaitingFor::Priority { .. }),
                "CR 123.3b: no choice for a non-owner: {:?}",
                state.waiting_for
            );
        }
        assert!(state.objects[&object].stickers.is_empty());
    }
}

/// CR 400.7 + CR 603.6: a PutSticker whose seeded object carries a zone-change
/// pin places nothing (and prompts nothing, even on the "up to" path) once
/// that object has left and returned; a current pin and an absent pin place
/// as before.
#[test]
fn put_sticker_honours_a_zone_change_pin_before_every_branch() {
    // (moved, pinned, count) → (stickers, waiting_for is a branch prompt)
    let run = |moved: bool, pinned: bool, count: QuantityExpr| -> (usize, bool) {
        let mut scenario = GameScenario::new();
        let object = scenario.add_creature(P0, "Turtle", 2, 2).id();
        let mut game = scenario.build();
        let state = game.state_mut();
        set_player_sticker_sheets(state, P0, &["Playable Delusionary Hydra".to_string()]);
        let mut ability = put_sticker(
            StickerKind::PowerToughness,
            count,
            vec![TargetRef::Object(object)],
            object,
        );
        if pinned {
            ability.target_incarnations =
                vec![ObjectIncarnationRef::from_object(&state.objects[&object])];
        }
        let mut events = Vec::<GameEvent>::new();
        if moved {
            move_to_zone(state, object, Zone::Exile, &mut events);
            move_to_zone(state, object, Zone::Battlefield, &mut events);
            assert_eq!(state.objects[&object].zone, Zone::Battlefield);
        }
        effects::stickers::resolve(state, &ability, &mut events).unwrap();
        (
            state.objects[&object].stickers.len(),
            matches!(state.waiting_for, WaitingFor::ChooseOneOfBranch { .. }),
        )
    };

    let observed = vec![
        ("current pin, Fixed 1", run(false, true, fixed(1))),
        ("current pin, up to 2", run(false, true, up_to(2))),
        ("moved, no pin, Fixed 1", run(true, false, fixed(1))),
        ("stale pin, Fixed 1", run(true, true, fixed(1))),
        ("stale pin, up to 2", run(true, true, up_to(2))),
    ];
    assert_eq!(
        observed,
        vec![
            // Reach-guards: a current pin places (Fixed 1) and prompts the
            // count (up to 2).
            ("current pin, Fixed 1", (1, false)),
            ("current pin, up to 2", (0, true)),
            // No pin recorded for the id: not held to a zone change.
            ("moved, no pin, Fixed 1", (1, false)),
            // CR 400.7: the pinned object left and returned — a new object —
            // so neither form places or prompts.
            ("stale pin, Fixed 1", (0, false)),
            ("stale pin, up to 2", (0, false)),
        ]
    );
}

/// CR 603.6 + CR 608.2c: a `ParentTarget` PutSticker selects its object from
/// the seeded targets only; with no seeded object it selects nothing (no
/// fallback to the ability's source), while a seeded object is stickered.
#[test]
fn put_sticker_parent_target_selects_only_a_seeded_object() {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Turtle", 2, 2).id();
    let mut game = scenario.build();
    let state = game.state_mut();
    set_player_sticker_sheets(state, P0, &["Playable Delusionary Hydra".to_string()]);

    let waiting_before = state.waiting_for.clone();
    let mut events = Vec::<GameEvent>::new();
    effects::stickers::resolve(
        state,
        &put_sticker(StickerKind::PowerToughness, fixed(1), Vec::new(), source),
        &mut events,
    )
    .unwrap();
    assert!(state.objects[&source].stickers.is_empty());
    assert_eq!(state.waiting_for, waiting_before);

    // Reach-guard: the same instruction with the source seeded places on it.
    effects::stickers::resolve(
        state,
        &put_sticker(
            StickerKind::PowerToughness,
            fixed(1),
            vec![TargetRef::Object(source)],
            source,
        ),
        &mut events,
    )
    .unwrap();
    assert_eq!(state.objects[&source].stickers.len(), 1);
}

fn prior_sticker() -> AppliedSticker {
    AppliedSticker::Name {
        locator: StickerLocator {
            sheet: "Ancestral Hot Dog Minotaur".into(),
            index: 1,
        },
        text: "Hot Dog".into(),
        position: 0,
        timestamp: 1,
    }
}

/// CR 608.2c: every PutSticker instruction starts with no "that sticker". A
/// put with no candidate sticker leaves none, even over a prior value; a put
/// that places latches exactly the sticker it placed.
#[test]
fn put_sticker_clears_that_sticker_before_each_instruction() {
    let mut scenario = GameScenario::new();
    let object = scenario.add_creature(P0, "Turtle", 2, 2).id();
    let mut game = scenario.build();
    let state = game.state_mut();
    let ability = put_sticker(
        StickerKind::PowerToughness,
        fixed(1),
        vec![TargetRef::Object(object)],
        object,
    );
    let mut events = Vec::<GameEvent>::new();

    // No sticker sheet → no candidate (CR 123.3): nothing placed, no record.
    state.placed_sticker_this_resolution = Some(prior_sticker());
    effects::stickers::resolve(state, &ability, &mut events).unwrap();
    assert!(state.objects[&object].stickers.is_empty());
    assert_eq!(state.placed_sticker_this_resolution, None);

    // Reach-guard: with a sheet, the single candidate is placed and latched.
    set_player_sticker_sheets(state, P0, &["Playable Delusionary Hydra".to_string()]);
    state.placed_sticker_this_resolution = Some(prior_sticker());
    effects::stickers::resolve(state, &ability, &mut events).unwrap();
    let placed = &state.objects[&object].stickers;
    assert_eq!(placed.len(), 1);
    assert_eq!(
        state.placed_sticker_this_resolution,
        Some(placed[0].clone())
    );
    assert_ne!(state.placed_sticker_this_resolution, Some(prior_sticker()));
}

/// CR 608.2c: a new top-level resolution cannot inherit a prior resolution's
/// "that sticker"; a nested (depth 1) continuation of the same resolution
/// keeps it.
#[test]
fn top_level_resolution_clears_that_sticker() {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Turtle", 2, 2).id();
    let mut game = scenario.build();
    let state = game.state_mut();
    let ability = ResolvedAbility::new(Effect::NoOp, Vec::new(), source, P0);
    let mut events = Vec::<GameEvent>::new();

    state.placed_sticker_this_resolution = Some(prior_sticker());
    effects::resolve_ability_chain(state, &ability, &mut events, 1).unwrap();
    assert_eq!(
        state.placed_sticker_this_resolution,
        Some(prior_sticker()),
        "a depth-1 continuation keeps the record"
    );

    effects::resolve_ability_chain(state, &ability, &mut events, 0).unwrap();
    assert_eq!(state.placed_sticker_this_resolution, None);
}

/// CR 123.6d + CR 123.6e: only a name sticker has name text to count letters on.
#[test]
fn applied_sticker_name_text_projects_only_name_stickers() {
    let locator = StickerLocator {
        sheet: "Ancestral Hot Dog Minotaur".into(),
        index: 0,
    };
    assert_eq!(prior_sticker().name_text(), Some("Hot Dog"));
    for other in [
        AppliedSticker::Ability {
            locator: locator.clone(),
            ticket_cost: 3,
            text: "Flying".into(),
            timestamp: 1,
        },
        AppliedSticker::PowerToughness {
            locator: locator.clone(),
            ticket_cost: 2,
            power: 1,
            toughness: 4,
            timestamp: 1,
        },
        AppliedSticker::Art {
            locator,
            label: "Ancestral Hot Dog Minotaur art 1".into(),
            timestamp: 1,
        },
    ] {
        assert_eq!(other.name_text(), None, "{other:?}");
    }
}

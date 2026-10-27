//! Exact result cohorts remain distinct from later Draw and linked-source piles.

use super::jev_defining_destroy::{cost, fund, settled};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::resolution::ResolutionStateWire;
use engine::types::zones::Zone;

const FUNERAL: &str = "When The Kenriths' Royal Funeral enters, exile up to two target legendary creature cards from your graveyard. You draw X cards and you lose X life, where X is the greatest mana value among cards exiled this way.\nLegendary spells you cast cost {1} less to cast for each card exiled with The Kenriths' Royal Funeral.";
const BLESSING: &str = "Draw X cards, where X is the greatest power among creatures you controlled as you cast this spell. If this spell was cast from exile, you gain twice X life.\nForetell {4}{G} (During your turn, you may pay {2} and exile this card from your hand face down. Cast it on a later turn for its foretell cost.)";
const STITCHER: &str = "{2}{U}, {T}: Each player mills three cards. Exile up to two creature cards put into graveyards this way. Create an X/X blue Zombie creature token, where X is the total power of the cards exiled this way.";
const SZAT: &str = "Choose one. If you control a commander as you cast this spell, you may choose both instead.\n• Each opponent sacrifices a creature they control with the greatest power.\n• Exile all opponents' graveyards, then create X 0/1 black Thrull creature tokens, where X is the greatest power among creature cards exiled this way.";

fn roundtrip(runner: GameRunner, wire: bool) -> GameRunner {
    if wire {
        let value =
            serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                .unwrap();
        GameRunner::from_state(
            serde_json::from_value::<ResolutionStateWire>(value)
                .unwrap()
                .into_game_state(),
        )
    } else {
        let bytes = serde_json::to_vec(runner.state()).unwrap();
        GameRunner::from_state(serde_json::from_slice::<GameState>(&bytes).unwrap())
    }
}

fn funeral_board() -> (
    GameRunner,
    ObjectId,
    [ObjectId; 3],
    ObjectId,
    ObjectId,
    ObjectId,
) {
    let parsed = engine::parser::oracle::parse_oracle_text(
        FUNERAL,
        "The Kenriths' Royal Funeral",
        &[],
        &["Enchantment".into()],
        &[],
    );
    assert!(
        parsed.parse_warnings.is_empty(),
        "full pinned Funeral has no parser gaps: {parsed:#?}"
    );
    assert!(!serde_json::to_string(&parsed)
        .unwrap()
        .contains("Unimplemented"));
    assert_eq!(parsed.triggers.len(), 1);
    assert_eq!(
        parsed.statics.len(),
        1,
        "independent linked-source discount is preserved"
    );
    let root = parsed.triggers[0].execute.as_deref().unwrap();
    let chain: Vec<_> =
        std::iter::successors(Some(root), |node| node.sub_ability.as_deref()).collect();
    assert!(chain.iter().any(|node| matches!(
        node.effect.as_ref(),
        engine::types::ability::Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Exile,
            ..
        }
    ) && node.declares_return_result.is_some()));
    assert!(chain.iter().any(|node| matches!(
        node.effect.as_ref(),
        engine::types::ability::Effect::Draw { .. }
    )));
    assert!(chain.iter().any(|node| matches!(
        node.effect.as_ref(),
        engine::types::ability::Effect::LoseLife { .. }
    )));
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let low = scenario
        .add_creature_to_graveyard(P0, "Legendary MV2", 2, 2)
        .as_legendary()
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let middle = scenario
        .add_creature_to_graveyard(P0, "Legendary MV3 unselected", 3, 3)
        .as_legendary()
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let high = scenario
        .add_creature_to_graveyard(P0, "Legendary MV4", 4, 4)
        .as_legendary()
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let nonlegendary = scenario
        .add_creature_to_graveyard(P0, "Nonlegendary MV8", 8, 8)
        .with_mana_cost(ManaCost::generic(8))
        .id();
    let foreign = scenario
        .add_creature_to_graveyard(P1, "Foreign legendary MV7", 7, 7)
        .as_legendary()
        .with_mana_cost(ManaCost::generic(7))
        .id();
    for i in 0..3 {
        scenario.add_card_to_library_top(P0, &format!("Funeral draw {i}"));
    }
    let drawn_decoy = scenario
        .add_spell_to_library_top(P0, "Drawn MV9 decoy", true)
        .with_mana_cost(ManaCost::generic(9))
        .id();
    let source = scenario
        .add_spell_to_hand_from_oracle(P0, "The Kenriths' Royal Funeral", false, FUNERAL)
        .as_enchantment()
        .as_legendary()
        .with_mana_cost(cost("{2}{W}{B}"))
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 2, &[ManaType::White, ManaType::Black]);
    runner.cast(source).commit();
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    runner.resolve_top();
    (
        runner,
        source,
        [low, middle, high],
        nonlegendary,
        foreign,
        drawn_decoy,
    )
}

fn assert_funeral_prompt(runner: &GameRunner, allowed: &[ObjectId], excluded: &[ObjectId]) {
    let WaitingFor::TriggerTargetSelection {
        player,
        target_slots,
        selection,
        source_id,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "actual paid Funeral ETB must announce targets: {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert!(source_id.is_some());
    let legal = &target_slots[selection.current_slot].legal_targets;
    for id in allowed {
        assert!(legal.contains(&TargetRef::Object(*id)), "eligible {id:?}");
    }
    for id in excluded {
        assert!(
            !legal.contains(&TargetRef::Object(*id)),
            "ineligible {id:?}"
        );
    }
}

#[test]
fn funeral_paid_etb_original_mv_four_survives_draw_at_both_canonical_readers() {
    // CR 608.2c / CR 608.2i: both X reads use the original qualifying exile,
    // even after Draw introduces a larger-mana-value card into another result.
    for wire in [false, true] {
        let (runner, source, cards, nonlegendary, foreign, drawn) = funeral_board();
        assert_funeral_prompt(&runner, &cards, &[nonlegendary, foreign]);
        let mut runner = roundtrip(runner, wire);
        assert_funeral_prompt(&runner, &cards, &[nonlegendary, foreign]);
        let before = serde_json::to_value(runner.state()).unwrap();
        assert!(engine::game::engine::apply(
            runner.state_mut(),
            P1,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(cards[2]))
            }
        )
        .is_err());
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(cards[2])),
            })
            .unwrap();
        assert_funeral_prompt(
            &runner,
            &[cards[0], cards[1]],
            &[cards[2], nonlegendary, foreign],
        );
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(cards[0])),
            })
            .unwrap();
        runner.advance_until_stack_empty();
        for id in [cards[0], cards[2]] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
        }
        for id in [cards[1], nonlegendary, foreign] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
        }
        assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
        assert_eq!(
            runner.state().players[0].hand.len(),
            4,
            "Draw4, including MV9 decoy"
        );
        assert_eq!(runner.life(P0), 16, "Lose4 uses the same original cohort");
        assert_eq!(runner.life(P1), 20);
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        settled(&runner);
    }
}

#[test]
fn funeral_completed_empty_and_late_illegal_are_distinct_from_all_targets_fizzling() {
    // CR 608.2b: all-target-illegal does not resolve; an explicitly empty
    // up-to choice does resolve and publishes its own completed empty result.
    for mode in [0, 1, 2] {
        let (mut runner, _, cards, nonlegendary, foreign, drawn) = funeral_board();
        assert_funeral_prompt(&runner, &cards, &[nonlegendary, foreign]);
        if mode == 0 {
            runner
                .act(GameAction::ChooseTarget { target: None })
                .unwrap();
        } else {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(cards[2])),
                })
                .unwrap();
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(cards[0])),
                })
                .unwrap();
            // Fixture-only opponent response before resolution, using the normal
            // zone primitive; the exile producer and its result remain untouched.
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                cards[2],
                Zone::Hand,
                &mut vec![],
            );
            if mode == 2 {
                engine::game::zones::move_to_zone(
                    runner.state_mut(),
                    cards[0],
                    Zone::Hand,
                    &mut vec![],
                );
            }
        }
        runner.advance_until_stack_empty();
        let expected = if mode == 1 { 2 } else { 0 };
        assert_eq!(runner.life(P0), 20 - expected);
        assert_eq!(
            runner.state().objects[&drawn].zone,
            if mode == 1 { Zone::Hand } else { Zone::Library }
        );
        if mode == 1 {
            assert_eq!(runner.state().objects[&cards[0]].zone, Zone::Exile);
        }
        settled(&runner);
    }
}

#[test]
fn blessing_locks_cast_power_for_draw_and_exile_only_double_life() {
    use engine::types::game_state::CastingVariant;
    for exile in [false, true] {
        for wire in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.with_library_top(P0, &["one", "two", "three", "four", "fifth"]);
            let greatest = scenario.add_creature(P0, "Cast-time four", 4, 7).id();
            let lesser = scenario.add_creature(P0, "Cast-time two", 2, 7).id();
            let spell = scenario
                .add_spell_to_hand_from_oracle(P0, "Lifestream's Blessing", false, BLESSING)
                .with_mana_cost(cost("{4}{G}{G}"))
                .id();
            let mut runner = scenario.build();
            if exile {
                fund(&mut runner, 2, &[]);
                runner
                    .act(GameAction::Foretell {
                        object_id: spell,
                        card_id: runner.state().objects[&spell].card_id,
                    })
                    .unwrap();
                assert_eq!(runner.state().objects[&spell].zone, Zone::Exile);
                assert!(runner.state().objects[&spell].foretold);
                runner.state_mut().turn_number += 1;
                fund(&mut runner, 4, &[ManaType::Green]);
                runner
                    .cast(spell)
                    .casting_variant(CastingVariant::Foretell)
                    .commit();
            } else {
                fund(&mut runner, 4, &[ManaType::Green; 2]);
                runner.cast(spell).commit();
            }
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
            let mut runner = roundtrip(runner, wire);
            engine::game::zones::move_to_zone(
                runner.state_mut(),
                greatest,
                Zone::Hand,
                &mut vec![],
            );
            // A changed surviving creature must not cause either scalar to reread.
            runner
                .state_mut()
                .objects
                .get_mut(&lesser)
                .unwrap()
                .base_power = Some(12);
            runner.state_mut().objects.get_mut(&lesser).unwrap().power = Some(12);
            runner.advance_until_stack_empty();
            assert_eq!(runner.state().players[0].library.len(), 1, "locked Draw4");
            assert_eq!(runner.life(P0), if exile { 28 } else { 20 });
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
            settled(&runner);
        }
    }
}

#[test]
fn blessing_zero_creatures_completes_with_the_published_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario.add_card_to_library_top(P0, "Zero-X library survivor");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Lifestream's Blessing", false, BLESSING)
        .with_mana_cost(cost("{4}{G}{G}"))
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 4, &[ManaType::Green; 2]);
    let outcome = runner.cast(spell).resolve();
    outcome.assert_hand_drawn(P0, 0);
    assert_eq!(runner.life(P0), 20);
    assert_eq!(runner.state().objects[&card].zone, Zone::Library);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    settled(&runner);
}

#[test]
fn stitcher_only_offers_creature_cards_from_its_actual_mill_and_keeps_creation_red() {
    for (empty, replaced_mill) in [(false, false), (true, false), (false, true)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_from_oracle(P0, "Stitcher Geralf", 3, 4, STITCHER)
            .as_legendary()
            .id();
        let old = scenario
            .add_creature_to_graveyard(P0, "Old graveyard power99", 99, 99)
            .id();
        if replaced_mill {
            scenario.add_enchantment_from_oracle(P0, "Rest in Peace", "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.");
        }
        let mut milled = vec![];
        for player in [P0, P1] {
            scenario.add_land_to_library_top(player, "Milled land A");
            scenario.add_land_to_library_top(player, "Milled land B");
            let id = scenario
                .add_spell_to_library_top(player, "Milled creature", false)
                .as_creature()
                .id();
            milled.push(id);
        }
        let mut runner = scenario.build();
        if empty {
            for id in &milled {
                runner
                    .state_mut()
                    .objects
                    .get_mut(id)
                    .unwrap()
                    .card_types
                    .core_types = vec![engine::types::card_type::CoreType::Sorcery];
            }
        }
        fund(&mut runner, 2, &[ManaType::Blue]);
        runner.activate(source, 0).resolve();
        if !empty && !replaced_mill {
            let WaitingFor::EffectZoneChoice {
                player,
                cards,
                zone: Zone::Graveyard,
                destination: Some(Zone::Exile),
                count: 2,
                ..
            } = &runner.state().waiting_for
            else {
                panic!(
                    "real just-milled exile choice: {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(*player, P0);
            assert_eq!(cards.len(), 2);
            assert!(milled.iter().all(|id| cards.contains(id)));
            assert!(!cards.contains(&old));
            assert!(runner
                .act(GameAction::SelectCards { cards: vec![old] })
                .is_err());
            runner
                .act(GameAction::SelectCards {
                    cards: vec![milled[1]],
                })
                .unwrap();
        }
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().players[0].library.len(), 0);
        assert_eq!(runner.state().players[1].library.len(), 0);
        assert_eq!(runner.state().objects[&old].zone, Zone::Graveyard);
        if !empty && !replaced_mill {
            assert_eq!(runner.state().objects[&milled[1]].zone, Zone::Exile);
        }
        if replaced_mill {
            for id in &milled {
                assert_eq!(
                    runner.state().objects[id].zone,
                    Zone::Exile,
                    "mill replacement is not an eligible Graveyard arrival"
                );
            }
        }
        assert!(
            !runner
                .state()
                .objects
                .values()
                .any(|object| object.zone == Zone::Battlefield
                    && object.is_token
                    && object.card_types.subtypes.contains(&"Zombie".to_owned())),
            "exact unsupported creation must not fabricate a token"
        );
        assert!(runner.state().objects[&source].tapped);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        settled(&runner);
    }
}

#[test]
fn szat_uses_opponents_graveyard_departure_power_and_excludes_own_decoys() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let own = scenario
        .add_creature_to_graveyard(P0, "Own power99", 99, 99)
        .id();
    let first = scenario
        .add_creature_to_graveyard(P1, "Opposing power4", 4, 4)
        .id();
    let second = scenario
        .add_creature_to_graveyard(P1, "Opposing power2", 2, 2)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Szat's Will", true, SZAT)
        .with_mana_cost(cost("{4}{B}"))
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 4, &[ManaType::Black]);
    runner.cast(spell).modes(&[1]).resolve();
    for id in [first, second] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
    }
    assert_eq!(runner.state().objects[&own].zone, Zone::Graveyard);
    let tokens: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.zone == Zone::Battlefield
                && object.is_token
                && object.card_types.subtypes.contains(&"Thrull".into())
        })
        .collect();
    assert_eq!(tokens.len(), 4);
    assert!(tokens.iter().all(|object| object.controller == P0
        && object.power == Some(0)
        && object.toughness == Some(1)));
    settled(&runner);
}

#[test]
fn szat_boneyard_wurm_uses_graveyard_power_before_the_exact_exile() {
    // CR 208.2a / CR 604.3: the Wurm's CDA works remotely; Szat's pinned
    // ruling expressly requires the power it had in the graveyard.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let worm = scenario.add_creature_to_graveyard(P1, "Boneyard Wurm", 0, 0)
        .from_oracle_text("Boneyard Wurm's power and toughness are each equal to the number of creature cards in your graveyard.").id();
    let other = scenario
        .add_creature_to_graveyard(P1, "Retained CDA partner", 1, 1)
        .id();
    let own = scenario
        .add_creature_to_graveyard(P0, "Own power99 decoy", 99, 99)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Szat's Will", true, SZAT)
        .with_mana_cost(cost("{4}{B}"))
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 4, &[ManaType::Black]);
    runner.cast(spell).modes(&[1]).resolve();
    for id in [worm, other] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
    }
    assert_eq!(runner.state().objects[&own].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&worm].power,
        Some(0),
        "current Exile CDA is a discriminating different value"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|object| object.is_token
                && object.zone == Zone::Battlefield
                && object.card_types.subtypes.contains(&"Thrull".into()))
            .count(),
        2
    );
    settled(&runner);
}

#[test]
fn szat_both_modes_take_the_opponents_actual_tie_choice_before_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Commander control witness", 1, 1)
        .commander();
    let first = scenario.add_creature(P1, "First greatest power", 5, 5).id();
    let chosen = scenario
        .add_creature(P1, "Nondefault greatest power", 5, 5)
        .id();
    let small = scenario.add_creature(P1, "Smaller survives", 2, 2).id();
    let prior = scenario
        .add_creature_to_graveyard(P1, "Earlier graveyard power3", 3, 3)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Szat's Will", true, SZAT)
        .with_mana_cost(cost("{4}{B}"))
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 4, &[ManaType::Black]);
    runner.cast(spell).modes(&[0, 1]).resolve();
    let WaitingFor::EffectZoneChoice { player, cards, .. } = &runner.state().waiting_for else {
        panic!("actual opposing player chooses among tied greatest creatures");
    };
    assert_eq!(*player, P1);
    assert_eq!(cards.len(), 2);
    assert!(cards.contains(&first) && cards.contains(&chosen));
    assert!(!cards.contains(&small));
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&chosen].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&prior].zone, Zone::Exile);
    for id in [first, small] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
    }
    assert_eq!(
        runner
            .state()
            .objects
            .values()
            .filter(|object| object.is_token
                && object.zone == Zone::Battlefield
                && object.card_types.subtypes.contains(&"Thrull".into()))
            .count(),
        5
    );
    settled(&runner);
}

#[test]
fn funeral_live_linked_discount_stops_counting_a_departed_exile_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let low = scenario
        .add_creature_to_graveyard(P0, "Legendary MV2", 2, 2)
        .as_legendary()
        .with_mana_cost(cost("{2}"))
        .id();
    let high = scenario
        .add_creature_to_graveyard(P0, "Legendary MV4", 4, 4)
        .as_legendary()
        .with_mana_cost(cost("{4}"))
        .id();
    scenario.with_library_top(P0, &["one", "two", "three", "four"]);
    let source = scenario
        .add_spell_to_hand_from_oracle(P0, "The Kenriths' Royal Funeral", false, FUNERAL)
        .as_enchantment()
        .as_legendary()
        .with_mana_cost(cost("{2}{W}{B}"))
        .id();
    let first = scenario
        .add_creature_to_hand(P0, "First legendary spell", 1, 1)
        .as_legendary()
        .with_mana_cost(cost("{3}"))
        .id();
    let second = scenario
        .add_creature_to_hand(P0, "Second legendary spell", 1, 1)
        .as_legendary()
        .with_mana_cost(cost("{3}"))
        .id();
    let third = scenario
        .add_creature_to_hand(P0, "After-reentry legendary spell", 1, 1)
        .as_legendary()
        .with_mana_cost(cost("{3}"))
        .id();
    let bounce = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disperse",
            true,
            "Return target nonland permanent to its owner's hand.",
        )
        .with_mana_cost(cost("{1}{U}"))
        .id();
    let pull = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Pull from Eternity",
            true,
            "Put target face-up exiled card into its owner's graveyard.",
        )
        .with_mana_cost(cost("{W}"))
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 2, &[ManaType::White, ManaType::Black]);
    runner.cast(source).target_objects(&[high, low]).resolve();
    assert_eq!(runner.life(P0), 16);
    for id in [low, high] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Exile);
    }
    fund(&mut runner, 1, &[]);
    runner.cast(first).resolve();
    assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "two live linked cards reduce generic3 to1"
    );
    fund(&mut runner, 0, &[ManaType::White]);
    runner.cast(pull).target_object(high).resolve();
    assert_eq!(runner.state().objects[&high].zone, Zone::Graveyard);
    fund(&mut runner, 2, &[]);
    runner.cast(second).resolve();
    assert_eq!(runner.state().objects[&second].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "only one live linked card now reduces generic3 to2"
    );
    assert_eq!(
        runner.life(P0),
        16,
        "ETB quantities already finished and cannot be retroactively rescanned"
    );
    settled(&runner);
    fund(&mut runner, 1, &[ManaType::Blue]);
    runner.cast(bounce).target_object(source).resolve();
    assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
    fund(&mut runner, 2, &[ManaType::White, ManaType::Black]);
    runner.cast(source).commit();
    runner.resolve_top();
    let WaitingFor::TriggerTargetSelection { player, .. } = &runner.state().waiting_for else {
        panic!("new Funeral incarnation has its own target announcement");
    };
    assert_eq!(*player, P0);
    runner
        .act(GameAction::ChooseTarget { target: None })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P0),
        16,
        "fresh completed-empty result cannot reuse the prior incarnation's MV4"
    );
    assert_eq!(runner.state().objects[&low].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&high].zone, Zone::Graveyard);
    fund(&mut runner, 3, &[]);
    runner.cast(third).resolve();
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "new incarnation does not borrow the old linked exile pile"
    );
    settled(&runner);
}

#[test]
fn funeral_result_owner_survives_actual_replacement_pause_and_rejects_missing_or_foreign_frames() {
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, ReplacementMode,
        TargetFilter,
    };
    use engine::types::game_state::ReturnResultProcessId;
    use engine::types::replacements::ReplacementEvent;
    for wire in [false, true] {
        for redirect in [false, true] {
            let (mut runner, _, cards, _, _, drawn) = funeral_board();
            // Secondary replacement fixture. The full printed Funeral and its
            // actual targeted exile still create the result and continuation.
            let replacement = ReplacementDefinition::new(ReplacementEvent::Moved)
                .valid_card(TargetFilter::SelfRef).destination_zone(Zone::Exile)
                .mode(ReplacementMode::Optional { decline: None })
                .execute(AbilityDefinition::new(AbilityKind::Spell,
                    serde_json::from_value::<Effect>(serde_json::json!({"type":"ChangeZone","origin":"Graveyard","destination":"Hand","target":{"type":"SelfRef"}})).unwrap()));
            let object = runner.state_mut().objects.get_mut(&cards[2]).unwrap();
            object.replacement_definitions.push(replacement.clone());
            std::sync::Arc::make_mut(&mut object.base_replacement_definitions).push(replacement);
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(cards[0])),
                })
                .unwrap();
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(cards[2])),
                })
                .unwrap();
            runner.advance_until_stack_empty();
            assert!(
                matches!(
                    runner.state().waiting_for,
                    WaitingFor::ReplacementChoice { player: P0, .. }
                ),
                "actual second qualifying exile is paused"
            );
            assert!(
                runner
                    .state()
                    .return_result_frames
                    .values()
                    .any(|frame| frame.process.is_some()),
                "owned current result authority exists before resuming"
            );
            let legal = engine::ai_support::legal_actions(runner.state());
            assert!(legal
                .iter()
                .any(|action| matches!(action, GameAction::ChooseReplacement { index: 0 })));
            assert!(
                legal
                    .iter()
                    .any(|action| matches!(action, GameAction::ChooseReplacement { index: 1 })),
                "AI exposes the actual nondefault decline"
            );
            let before_wrong_actor = serde_json::to_value(runner.state()).unwrap();
            assert!(engine::game::engine::apply(
                runner.state_mut(),
                P1,
                GameAction::ChooseReplacement { index: 1 }
            )
            .is_err());
            assert_eq!(
                serde_json::to_value(runner.state()).unwrap(),
                before_wrong_actor
            );
            for viewer in [Some(P0), Some(P1), None] {
                let filtered = match viewer {
                    Some(player) => {
                        engine::game::visibility::filter_state_for_viewer(runner.state(), player)
                    }
                    None => {
                        engine::game::visibility::filter_state_for_unseated_viewer(runner.state())
                    }
                };
                assert!(filtered.return_result_frames.is_empty());
                for envelope in [
                    serde_json::to_string(&filtered).unwrap(),
                    serde_json::to_string(&engine::game::derived_views::ClientGameStateRef::wrap(
                        runner.state(),
                        viewer,
                    ))
                    .unwrap(),
                ] {
                    for private in ["return_result_process", "return_result_occurrence"] {
                        assert!(
                            !envelope.contains(private),
                            "private result custody is absent from both real public envelopes"
                        );
                    }
                }
            }
            for malformed in 0..3 {
                let mut bad = runner.state().clone();
                if malformed == 0 {
                    bad.return_result_frames.clear();
                } else if malformed == 1 {
                    for frame in bad.return_result_frames.values_mut() {
                        frame.process = Some(ReturnResultProcessId(999_999));
                    }
                } else {
                    let original = *bad.return_result_frames.keys().next().unwrap();
                    let frame = bad.return_result_frames.remove(&original).unwrap();
                    bad.return_result_frames.insert(
                        engine::types::game_state::ReturnResultOccurrenceId(999_999),
                        frame,
                    );
                }
                assert!(
                    serde_json::from_value::<GameState>(serde_json::to_value(&bad).unwrap())
                        .is_err()
                );
                assert!(serde_json::from_value::<ResolutionStateWire>(
                    serde_json::to_value(ResolutionStateWire::from_game_state(bad)).unwrap()
                )
                .is_err());
            }
            fn incompatible_aggregate(value: &mut serde_json::Value) -> usize {
                match value {
                    serde_json::Value::Object(fields) => {
                        let own = usize::from(
                            fields.get("type").and_then(serde_json::Value::as_str)
                                == Some("ZoneChangeAggregate"),
                        );
                        if own == 1 {
                            fields.insert(
                                "from".into(),
                                serde_json::Value::String("Battlefield".into()),
                            );
                        }
                        own + fields
                            .values_mut()
                            .map(incompatible_aggregate)
                            .sum::<usize>()
                    }
                    serde_json::Value::Array(values) => {
                        values.iter_mut().map(incompatible_aggregate).sum()
                    }
                    _ => 0,
                }
            }
            let mut incompatible_raw = serde_json::to_value(runner.state()).unwrap();
            assert!(
                incompatible_aggregate(&mut incompatible_raw) >= 2,
                "both original result readers are retained in the paused complete program"
            );
            assert!(serde_json::from_value::<GameState>(incompatible_raw).is_err());
            let mut incompatible_wire =
                serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
                    .unwrap();
            assert!(incompatible_aggregate(&mut incompatible_wire) >= 2);
            assert!(serde_json::from_value::<ResolutionStateWire>(incompatible_wire).is_err());
            let mut runner = roundtrip(runner, wire);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { player: P0, .. }
            ));
            // The nondefault decline retains the normal exile; the accept arm
            // redirects it and therefore contributes no Graveyard→Exile record.
            runner
                .act(GameAction::ChooseReplacement {
                    index: if redirect { 0 } else { 1 },
                })
                .unwrap();
            runner.advance_until_stack_empty();
            let amount = if redirect { 2 } else { 4 };
            assert_eq!(runner.life(P0), 20 - amount);
            assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
            assert_eq!(runner.state().objects[&cards[0]].zone, Zone::Exile);
            assert_eq!(
                runner.state().objects[&cards[2]].zone,
                if redirect { Zone::Hand } else { Zone::Exile }
            );
            settled(&runner);
        }
    }
}

#[test]
fn splintering_wind_grants_the_actual_created_token_one_upkeep_and_flying() {
    use engine::types::counter::CounterType;
    use engine::types::keywords::Keyword;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["one", "two", "three"]);
    scenario.with_library_top(P1, &["one", "two", "three"]);
    let source = scenario.add_enchantment_from_oracle(P0, "Splintering Wind",
        "{2}{G}: This enchantment deals 1 damage to target creature. Create a 1/1 green Splinter creature token. It has flying and \"Cumulative upkeep {G}.\" When it leaves the battlefield, it deals 1 damage to you and each creature you control. (At the beginning of its controller's upkeep, that player puts an age counter on it, then sacrifices it unless they pay its upkeep cost for each age counter on it.)").id();
    let victim = scenario.add_creature(P1, "Damage recipient", 2, 5).id();
    let old = scenario
        .add_creature(P0, "Unrelated prior Splinter", 1, 3)
        .with_subtypes(vec!["Splinter"])
        .id();
    let mut runner = scenario.build();
    fund(&mut runner, 2, &[ManaType::Green]);
    runner.activate(source, 0).target_object(victim).resolve();
    assert_eq!(runner.state().objects[&victim].damage_marked, 1);
    let created: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token
                && object.zone == Zone::Battlefield
                && object.card_types.subtypes.contains(&"Splinter".into())
        })
        .map(|object| object.id)
        .collect();
    assert_eq!(created.len(), 1);
    let token = created[0];
    assert!(runner.state().objects[&token]
        .keywords
        .contains(&Keyword::Flying));
    assert_eq!(
        runner.state().objects[&token]
            .keywords
            .iter()
            .filter(|keyword| matches!(keyword, Keyword::CumulativeUpkeep(_)))
            .count(),
        1
    );
    assert!(!runner.state().objects[&old]
        .keywords
        .iter()
        .any(|keyword| matches!(keyword, Keyword::CumulativeUpkeep(_))));
    settled(&runner);
    for _ in 0..4 {
        runner.advance_to_end_step();
        runner.advance_to_upkeep();
        if runner.state().active_player == P0 {
            break;
        }
        runner.advance_to_phase(Phase::PreCombatMain);
    }
    runner.advance_until_stack_empty();
    let WaitingFor::UnlessPayment { player, .. } = &runner.state().waiting_for else {
        panic!("actual created token cumulative upkeep prompt");
    };
    assert_eq!(*player, P0);
    assert_eq!(
        runner.state().objects[&token]
            .counters
            .get(&CounterType::Age),
        Some(&1)
    );
    fund(&mut runner, 0, &[ManaType::Green]);
    runner.act(GameAction::PayUnlessCost { pay: true }).unwrap();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&token].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    settled(&runner);
    for _ in 0..4 {
        runner.advance_to_phase(Phase::PreCombatMain);
        runner.advance_to_end_step();
        runner.advance_to_upkeep();
        if runner.state().active_player == P0 {
            break;
        }
    }
    runner.advance_until_stack_empty();
    let WaitingFor::UnlessPayment { player, .. } = &runner.state().waiting_for else {
        panic!("second real cumulative upkeep");
    };
    assert_eq!(*player, P0);
    assert_eq!(
        runner.state().objects[&token]
            .counters
            .get(&CounterType::Age),
        Some(&2)
    );
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .unwrap();
    runner.advance_until_stack_empty();
    assert_ne!(runner.state().objects[&token].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&old].zone, Zone::Battlefield);
    assert_eq!(
        runner.life(P0),
        19,
        "created token's independent departure instruction still resolves"
    );
    settled(&runner);
}

#[test]
fn doors_scry_nonidentity_then_returned_dwarf_elf_gets_both_grants() {
    use engine::game::combat::AttackTarget;
    use engine::types::keywords::Keyword;
    for wire in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_artifact_from_oracle(P0, "Doors of Durin",
            "Whenever you attack, scry 2, then you may reveal the top card of your library. If a creature card is revealed this way, put it onto the battlefield tapped and attacking. Until your next turn, it gains trample if you control a Dwarf and hexproof if you control an Elf.").as_legendary().id();
        let attacker = scenario.add_creature(P0, "Attack source", 2, 3).id();
        let returned = scenario
            .add_spell_to_library_top(P0, "Returned Dwarf Elf", false)
            .as_creature()
            .with_subtypes(vec!["Dwarf", "Elf"])
            .id();
        let decoy = scenario
            .add_spell_to_library_top(P0, "Top noncreature decoy", true)
            .id();
        let mut runner = scenario.build();
        // A library creature needs ordinary printed combat characteristics.
        {
            let object = runner.state_mut().objects.get_mut(&returned).unwrap();
            object.power = Some(3);
            object.toughness = Some(4);
            object.base_power = Some(3);
            object.base_toughness = Some(4);
        }
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
            .unwrap();
        runner.advance_until_stack_empty();
        let WaitingFor::ScryChoice { player, cards, .. } = &runner.state().waiting_for else {
            panic!("actual Doors scry2");
        };
        assert_eq!(*player, P0);
        assert_eq!(cards, &vec![decoy, returned]);
        let mut runner = roundtrip(runner, wire);
        runner
            .act(GameAction::SelectCards {
                cards: vec![returned, decoy],
            })
            .unwrap();
        let WaitingFor::OptionalEffectChoice { player, .. } = &runner.state().waiting_for else {
            panic!("actual Doors reveal choice");
        };
        assert_eq!(*player, P0);
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .unwrap();
        runner.advance_until_stack_empty();
        let object = &runner.state().objects[&returned];
        assert_eq!(object.zone, Zone::Battlefield);
        assert!(object.tapped);
        assert!(object.keywords.contains(&Keyword::Trample));
        assert!(object.keywords.contains(&Keyword::Hexproof));
        for id in [source, attacker] {
            assert!(!runner.state().objects[&id]
                .keywords
                .contains(&Keyword::Trample));
            assert!(!runner.state().objects[&id]
                .keywords
                .contains(&Keyword::Hexproof));
        }
        assert_eq!(runner.state().objects[&decoy].zone, Zone::Library);
        settled(&runner);
    }
}

#[test]
fn dance_atomic_gap_preserves_actual_paid_shuffle_without_inventing_exile_or_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["one", "two", "three", "four", "five", "six"]);
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Dance with Calamity", false,
        "Shuffle your library. As many times as you choose, you may exile the top card of your library. If the total mana value of the cards exiled this way is 13 or less, you may cast any number of spells from among those cards without paying their mana costs.").with_mana_cost(cost("{7}{R}")).id();
    let mut runner = scenario.build();
    let before = runner.state().players[0].library.clone();
    fund(&mut runner, 7, &[ManaType::Red]);
    let outcome = runner.cast(spell).resolve();
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            engine::types::events::GameEvent::EffectResolved {
                kind: engine::types::ability::EffectKind::Shuffle,
                ..
            }
        )),
        "independent Shuffle actually executes"
    );
    assert_eq!(runner.state().players[0].library.len(), before.len());
    assert!(before
        .iter()
        .all(|id| runner.state().objects[id].zone == Zone::Library));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    settled(&runner);
}

#[test]
fn targeted_emblem_gaps_never_create_a_caster_emblem_beside_real_nonemblem_modes() {
    for (name, subtype, oracle, index) in [
        ("Garruk, Apex Predator", "Garruk", "[+1]: Destroy another target planeswalker.\n[+1]: Create a 3/3 black Beast creature token with deathtouch.\n[−3]: Destroy target creature. You gain life equal to its toughness.\n[−8]: Target opponent gets an emblem with \"Whenever a creature attacks you, it gets +5/+5 and gains trample until end of turn.\"", 3),
        ("Ob Nixilis Reignited", "Nixilis", "[+1]: You draw a card and you lose 1 life.\n[−3]: Destroy target creature.\n[−8]: Target opponent gets an emblem with \"Whenever a player draws a card, you lose 2 life.\"", 2),
        ("Rowan Kenrith", "Rowan", "[+2]: During target player's next turn, each creature that player controls attacks if able.\n[−2]: Rowan Kenrith deals 3 damage to each tapped creature target player controls.\n[−8]: Target player gets an emblem with \"Whenever you activate an ability that isn't a mana ability, copy it. You may choose new targets for the copy.\"\nPartner with Will Kenrith\nRowan Kenrith can be your commander.", 2),
        ("Will Kenrith", "Will", "[+2]: Until your next turn, up to two target creatures each have base power and toughness 0/3 and lose all abilities.\n[−2]: Target player draws two cards. Until your next turn, instant, sorcery, and planeswalker spells that player casts cost {2} less to cast.\n[−8]: Target player gets an emblem with \"Whenever you cast an instant or sorcery spell, copy it. You may choose new targets for the copy.\"\nPartner with Rowan Kenrith\nWill Kenrith can be your commander.", 2),
    ] {
        for emblem_mode in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.with_library_top(P0, &["Draw one", "Draw two"]);
            scenario.with_library_top(P1, &["Other draw one", "Other draw two"]);
            let source = scenario.add_planeswalker_from_oracle(P0, name, subtype, 10, oracle).id();
            let victim = scenario.add_creature(P1, "Tapped target", 2, 6).id();
            let mut runner = scenario.build();
            runner.state_mut().objects.get_mut(&victim).unwrap().tapped = true;
            if emblem_mode {
                runner.activate(source, index).target_player(P1).resolve();
                assert_eq!(runner.state().objects[&source].counters.get(&engine::types::counter::CounterType::Loyalty), Some(&2));
            } else {
                match name {
                    "Garruk, Apex Predator" => {
                        runner.activate(source, 1).resolve();
                        assert!(runner.state().objects.values().any(|object| object.is_token && object.zone == Zone::Battlefield && object.card_types.subtypes.contains(&"Beast".into())));
                    }
                    "Ob Nixilis Reignited" => {
                        runner.activate(source, 0).resolve();
                        assert_eq!(runner.state().players[0].hand.len(), 1);
                        assert_eq!(runner.life(P0), 19);
                    }
                    "Rowan Kenrith" => {
                        runner.activate(source, 1).target_player(P1).resolve();
                        assert_eq!(runner.state().objects[&victim].damage_marked, 3);
                    }
                    "Will Kenrith" => {
                        runner.activate(source, 1).target_player(P1).resolve();
                        assert_eq!(runner.state().players[1].hand.len(), 2);
                    }
                    _ => unreachable!(),
                }
            }
            assert!(!runner.state().objects.values().any(|object| object.is_emblem), "targeted gap cannot create a caster-only emblem");
            settled(&runner);
        }
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_planeswalker_from_oracle(P0, "Elspeth, Sun's Champion", "Elspeth", 8,
        "[+1]: Create three 1/1 white Soldier creature tokens.\n[−3]: Destroy all creatures with power 4 or greater.\n[−7]: You get an emblem with \"Creatures you control get +2/+2 and have flying.\"").id();
    let mut runner = scenario.build();
    runner.activate(source, 2).resolve();
    let emblems: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.is_emblem)
        .collect();
    assert_eq!(emblems.len(), 1, "ordinary self-emblem positive control");
    assert_eq!(emblems[0].owner, P0);
    assert_eq!(emblems[0].controller, P0);
    settled(&runner);
}

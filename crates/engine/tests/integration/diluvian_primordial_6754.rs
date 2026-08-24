//! Diluvian Primordial: paired opponent-graveyard targets become one fixed
//! during-resolution free-cast pool, with no graveyard substitution.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, Effect, SpellStackToGraveyardReplacement, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastOfferKind, CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const P2: PlayerId = PlayerId(2);
const DILUVIAN_ORACLE: &str = "Flying\nWhen this creature enters, for each opponent, you may cast up to one target instant or sorcery card from that player's graveyard without paying its mana cost. If a spell cast this way would be put into a graveyard, exile it instead.";
const DILUVIAN_WITH_DRAW_ORACLE: &str = "Flying\nWhen this creature enters, for each opponent, you may cast up to one target instant or sorcery card from that player's graveyard without paying its mana cost. If a spell cast this way would be put into a graveyard, exile it instead. Then draw a card.";

fn advance_to_trigger_target_selection(runner: &mut GameRunner) {
    for _ in 0..32 {
        match runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass while reaching Diluvian trigger");
            }
            ref other => panic!("unexpected state while reaching Diluvian trigger: {other:?}"),
        }
    }
    panic!("Diluvian Primordial ETB never reached its target prompt");
}

fn advance_to_free_cast_window(runner: &mut GameRunner) {
    for _ in 0..32 {
        match runner.state().waiting_for {
            WaitingFor::CastOffer {
                kind: CastOfferKind::FreeCastWindow { .. },
                ..
            } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass while resolving Diluvian trigger");
            }
            ref other => panic!("unexpected state while reaching Diluvian window: {other:?}"),
        }
    }
    panic!("Diluvian Primordial never opened its free-cast window");
}

// CR 115.10a: the per-opponent binder slots are pinned by construction and are
// announced by the engine, so the controller submits only the card targets.
fn choose_paired_targets(runner: &mut GameRunner, p1_card: ObjectId, p2_card: ObjectId) {
    for target in [TargetRef::Object(p1_card), TargetRef::Object(p2_card)] {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(target),
            })
            .expect("paired Diluvian target must be legal");
    }
}

fn assert_no_unimplemented_effects(ability: &AbilityDefinition) {
    assert!(
        !matches!(*ability.effect, Effect::Unimplemented { .. }),
        "test spell must use a supported effect, not Unimplemented"
    );
    if let Some(sub) = &ability.sub_ability {
        assert_no_unimplemented_effects(sub);
    }
    if let Some(otherwise) = &ability.else_ability {
        assert_no_unimplemented_effects(otherwise);
    }
}

/// CR 115.1d + CR 608.2g + CR 614.1a: The real cast/trigger pipeline chooses
/// one card per opponent, snapshots exactly those cards into FreeCastWindow,
/// casts one at zero mana, re-offers only the remaining approved card, then
/// exiles the successfully cast spell while declined and unselected cards stay
/// in their owners' graveyards. Each negative assertion has the selected-card
/// reach guard immediately above it.
#[test]
fn diluvian_primordial_uses_selected_pairs_as_its_fixed_free_cast_pool() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    // The cast spell is a cantrip. Seed P0's library so resolving it proves
    // the full cast path deterministically rather than relying on an empty
    // library draw-loss implementation detail.
    let p0_draw_filler = scenario.add_card_to_library_top(P0, "P0 Draw Filler");
    let primordial = scenario
        .add_creature_to_hand_from_oracle(P0, "Diluvian Primordial", 5, 5, DILUVIAN_ORACLE)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let p1_selected = scenario
        .add_spell_to_graveyard(P1, "P1 Selected", true)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("Draw a card.")
        .id();
    let p2_selected = scenario
        .add_spell_to_graveyard(P2, "P2 Selected", false)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text("Draw a card.")
        .id();
    let p1_extra = scenario
        .add_spell_to_graveyard(P1, "P1 Unselected", true)
        .from_oracle_text("Draw a card.")
        .id();
    let p2_extra = scenario
        .add_spell_to_graveyard(P2, "P2 Unselected", false)
        .from_oracle_text("Draw a card.")
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&primordial].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: primordial,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Diluvian Primordial must succeed");
    advance_to_trigger_target_selection(&mut runner);
    choose_paired_targets(&mut runner, p1_selected, p2_selected);
    advance_to_free_cast_window(&mut runner);

    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            player: P0,
            kind:
                CastOfferKind::FreeCastWindow {
                    candidates,
                    remaining_casts,
                    member_pool,
                    graveyard_replacement,
                    ..
                },
        } => {
            assert_eq!(member_pool, vec![p1_selected, p2_selected]);
            assert_eq!(candidates, vec![p1_selected, p2_selected]);
            assert_eq!(remaining_casts, 2);
            assert_eq!(
                graveyard_replacement,
                Some(SpellStackToGraveyardReplacement::Exile)
            );
        }
        other => panic!("expected Diluvian FreeCastWindow, got {other:?}"),
    }

    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(p1_selected),
        })
        .expect("selected P1 spell must cast for free");
    assert_eq!(runner.state().objects[&p1_selected].zone, Zone::Stack);
    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            kind:
                CastOfferKind::FreeCastWindow {
                    candidates,
                    member_pool,
                    ..
                },
            ..
        } => {
            assert_eq!(member_pool, vec![p1_selected, p2_selected]);
            assert_eq!(candidates, vec![p2_selected]);
        }
        other => panic!("expected fixed-pool re-offer, got {other:?}"),
    }
    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the remaining approved card must finish the window");
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&p1_selected].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&p0_draw_filler].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&p2_selected].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&p1_extra].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&p2_extra].zone, Zone::Graveyard);
}

/// CR 903.4 + CR 903.5c + CR 608.2g: Color identity constrains deck
/// construction, not casting permissions granted during a game. A blue
/// Commander player may therefore cast the red opponent spell selected for
/// Diluvian Primordial's free-cast window.
#[test]
fn diluvian_primordial_casts_off_identity_opponent_spell_in_commander() {
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let commander = scenario
        .add_creature_to_hand(P0, "Blue Commander", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    scenario.with_commander(commander);
    let primordial = scenario
        .add_creature_to_hand_from_oracle(P0, "Diluvian Primordial", 5, 5, DILUVIAN_ORACLE)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let red_opponent_spell = scenario
        .add_spell_to_graveyard(P1, "Red Opponent Spell", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text("Draw a card.")
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&primordial].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: primordial,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Diluvian Primordial must succeed");
    advance_to_trigger_target_selection(&mut runner);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(red_opponent_spell)),
        })
        .expect("opponent red instant must be a legal Diluvian target");
    advance_to_free_cast_window(&mut runner);

    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(red_opponent_spell),
        })
        .expect("Diluvian must cast the selected red opponent spell despite P0's blue identity");
    assert_eq!(
        runner.state().objects[&red_opponent_spell].zone,
        Zone::Stack
    );
}

/// CR 608.2b + CR 608.2g: A paired target that leaves its graveyard before
/// resolution is removed from the fixed pool. The still-legal paired target
/// positively reaches the window; its same-owner extra proves the resolver did
/// not substitute after revalidation.
#[test]
fn diluvian_primordial_drops_removed_pair_without_graveyard_substitution() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let primordial = scenario
        .add_creature_to_hand_from_oracle(P0, "Diluvian Primordial", 5, 5, DILUVIAN_ORACLE)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let p1_selected = scenario
        .add_spell_to_graveyard(P1, "P1 Selected", true)
        .from_oracle_text("Draw a card.")
        .id();
    let p2_removed = scenario
        .add_spell_to_graveyard(P2, "P2 Removed", false)
        .from_oracle_text("Draw a card.")
        .id();
    let p2_extra = scenario
        .add_spell_to_graveyard(P2, "P2 Extra", false)
        .from_oracle_text("Draw a card.")
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&primordial].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: primordial,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Diluvian Primordial must succeed");
    advance_to_trigger_target_selection(&mut runner);
    choose_paired_targets(&mut runner, p1_selected, p2_removed);

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), p2_removed, Zone::Exile, &mut events);
    advance_to_free_cast_window(&mut runner);
    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            kind:
                CastOfferKind::FreeCastWindow {
                    candidates,
                    member_pool,
                    ..
                },
            ..
        } => {
            assert_eq!(member_pool, vec![p1_selected]);
            assert_eq!(candidates, vec![p1_selected]);
            assert!(!candidates.contains(&p2_extra));
        }
        other => panic!("expected surviving paired target window, got {other:?}"),
    }
}

/// CR 115.1d + CR 603.3d + CR 608.2g: An opponent with no legal instant or
/// sorcery target contributes no paired slots, but that omission must not
/// suppress another opponent's independently selected target or its normal
/// cast pipeline.
#[test]
fn diluvian_primordial_skips_targetless_opponent_and_casts_other_selected_spell() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    // As above, make the selected cantrip's resolution observable without an
    // empty-library draw-loss side effect.
    let p0_draw_filler = scenario.add_card_to_library_top(P0, "P0 Draw Filler");
    let primordial = scenario
        .add_creature_to_hand_from_oracle(P0, "Diluvian Primordial", 5, 5, DILUVIAN_ORACLE)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let p2_selected = scenario
        .add_spell_to_graveyard(P2, "P2 Selected", true)
        .from_oracle_text("Draw a card.")
        .id();
    let p2_extra = scenario
        .add_spell_to_graveyard(P2, "P2 Unselected", false)
        .from_oracle_text("Draw a card.")
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&primordial].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: primordial,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Diluvian Primordial must succeed");
    advance_to_trigger_target_selection(&mut runner);

    match runner.state().waiting_for.clone() {
        WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            ..
        } => {
            assert_eq!(
                target_slots.len(),
                2,
                "the targetless P1 must contribute no unusable player/object pair"
            );
            assert_eq!(target_slots[0].legal_targets, vec![TargetRef::Player(P2)]);
            assert_eq!(
                target_slots[1].legal_targets,
                vec![TargetRef::Object(p2_selected), TargetRef::Object(p2_extra)]
            );
            // CR 115.10a: the binder slot still exists, but the engine
            // announces it — the first prompt is the card, not the opponent.
            assert_eq!(selection.current_slot, 1);
            assert_eq!(
                selection.current_legal_targets,
                vec![TargetRef::Object(p2_selected), TargetRef::Object(p2_extra)]
            );
        }
        other => panic!("expected Diluvian target selection, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(p2_selected)),
        })
        .expect("P2's paired target must remain selectable when P1 has none");
    advance_to_free_cast_window(&mut runner);
    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            kind:
                CastOfferKind::FreeCastWindow {
                    candidates,
                    member_pool,
                    remaining_casts,
                    ..
                },
            ..
        } => {
            assert_eq!(member_pool, vec![p2_selected]);
            assert_eq!(candidates, vec![p2_selected]);
            assert_eq!(remaining_casts, 1);
        }
        other => panic!("expected P2-only Diluvian FreeCastWindow, got {other:?}"),
    }
    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(p2_selected),
        })
        .expect("P2's selected spell must cast through the free-cast window");
    assert_eq!(runner.state().objects[&p2_selected].zone, Zone::Stack);
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&p2_selected].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&p0_draw_filler].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&p2_extra].zone, Zone::Graveyard);
}

/// CR 608.2c + CR 608.2g: the destination-replacement rider is metadata for
/// the free-cast window, but a direct sibling after it remains a later printed
/// instruction. It must wait until the controller closes the window rather
/// than resolving while the cast offer is still active.
#[test]
fn diluvian_primordial_defers_sibling_after_open_free_cast_window() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let draw_filler = scenario.add_card_to_library_top(P0, "Deferred Draw Filler");
    let primordial = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Diluvian Primordial",
            5,
            5,
            DILUVIAN_WITH_DRAW_ORACLE,
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let selected = scenario
        .add_spell_to_graveyard(P1, "Selected Spell", true)
        .from_oracle_text("Draw a card.")
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&primordial].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: primordial,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Diluvian Primordial must succeed");
    advance_to_trigger_target_selection(&mut runner);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(selected)),
        })
        .expect("Diluvian's paired target must be legal");
    advance_to_free_cast_window(&mut runner);

    assert_eq!(
        runner.state().objects[&draw_filler].zone,
        Zone::Library,
        "the sibling draw must remain deferred until the free-cast window closes"
    );
    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the window must close it");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&draw_filler].zone, Zone::Hand);
}

/// CR 601.2c + CR 608.2c: a selected spell can be a legal Diluvian target even
/// when it has no legal targets to cast. That suppresses the free-cast window,
/// but must neither move the selected card nor suppress the printed tail.
#[test]
fn diluvian_primordial_uncastable_selected_spell_stays_in_graveyard_and_runs_tail() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let draw_filler = scenario.add_card_to_library_top(P0, "Immediate Draw Filler");
    let primordial = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Diluvian Primordial",
            5,
            5,
            DILUVIAN_WITH_DRAW_ORACLE,
        )
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let selected = scenario
        .add_spell_to_graveyard(P1, "No Artifact Target", true)
        .from_oracle_text("Destroy target artifact.")
        .id();

    let mut runner = scenario.build();
    let selected_object = &runner.state().objects[&selected];
    assert_no_unimplemented_effects(&selected_object.abilities[0]);
    assert!(
        !engine::game::casting::spell_has_legal_targets(runner.state(), selected_object, P0),
        "Destroy target artifact must be uncastable before the ETB resolves when no artifact exists"
    );
    let card_id = runner.state().objects[&primordial].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: primordial,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Diluvian Primordial must succeed");
    advance_to_trigger_target_selection(&mut runner);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(selected)),
        })
        .expect("the selected uncastable spell must still be a legal Diluvian pair");
    runner.advance_until_stack_empty();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::FreeCastWindow { .. },
                ..
            }
        ),
        "an uncastable selected spell must not open a FreeCastWindow"
    );
    assert_eq!(runner.state().objects[&selected].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&draw_filler].zone, Zone::Hand);
}

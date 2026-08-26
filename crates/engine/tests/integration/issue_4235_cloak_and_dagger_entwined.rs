//! Issue #4235: Cloak and Dagger, Entwined — plural "leave the battlefield"
//! duration parsing, per-slot targeting, and heterogeneous exile choice.
//!
//! Oracle text (Marvel Spider-Man set MSH, read from `client/public/card-data.json`):
//!   "Deathtouch, lifelink
//!    When Cloak and Dagger enter, choose target opponent and up to one target
//!    creature they control. They reveal their hand. You may exile a nonland
//!    card from their hand or the chosen creature until Cloak and Dagger leave
//!    the battlefield."
//!
//! Three findings, matching the maintainer review on PR #5871:
//!
//! 1. THE ORIGINAL BUG: `parse_until_body` (the "until X leaves the
//!    battlefield" duration combinator in `parser/oracle_nom/duration.rs`)
//!    only matched the singular verb form "leaves the battlefield". A card
//!    whose own name is a plural subject ("Cloak and Dagger") prints plural
//!    agreement — "until Cloak and Dagger leave the battlefield" — which
//!    never matched, so the exile's `duration` silently stayed `None` and no
//!    `ExileLinkKind::UntilSourceLeaves` link was created. Fixed by accepting
//!    both verb forms (CR 611.2a).
//!
//! 2. THE INTERACTIVE-PATH GAP (review blocker 1): the duration was ALSO
//!    dropped whenever the exile had more than one eligible candidate —
//!    `WaitingFor::EffectZoneChoice` carried no `duration` field, so the
//!    resume authority (`engine_resolution_choices.rs`) reconstructed
//!    `ChangeZoneIterationCtx` with `duration: None`. Fixed by carrying the
//!    duration across the round-trip; the two-candidate runtime test below
//!    proves the chosen card's exile link survives an interactive selection
//!    and the card returns when the source leaves.
//!
//! 3. The full printed sentence now uses two independent target slots: an
//!    exact-one opponent and an up-to-one creature that opponent controls.
//!    The reveal binds slot 0, and one optional zone choice unions a nonland
//!    card in that opponent's hand with the chosen creature in slot 1.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    ControllerRef, Duration, Effect, FilterProp, MultiTargetSpec, QuantityExpr, TargetFilter,
    TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{ExileLinkKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

/// Cloak and Dagger's full, real printed text.
const CLOAK_AND_DAGGER_FULL: &str = "Deathtouch, lifelink\n\
When Cloak and Dagger enter, choose target opponent and up to one target creature they control. \
They reveal their hand. You may exile a nonland card from their hand or the chosen creature \
until Cloak and Dagger leave the battlefield.";

/// The SUPPORTED single-referent subset of the same idiom, with the same
/// plural-name verb agreement ("Cloak and Dagger ... leave"): no "chosen
/// creature" alternative, no secondary creature target. This is the shape the
/// duration fix and the interactive carry-through are exercised against.
const CLOAK_AND_DAGGER_SUPPORTED_SUBSET: &str = "Deathtouch, lifelink\n\
When Cloak and Dagger enter, target opponent reveals their hand. You may exile a nonland \
card from their hand until Cloak and Dagger leave the battlefield.";

/// AST-shape regression for the plural-verb duration fix: on the supported
/// subset, the exile sub-ability must carry `Duration::UntilHostLeavesPlay`,
/// not silently drop to `None`.
#[test]
fn plural_leave_duration_parses_on_supported_subset() {
    let parsed = parse_oracle_text(
        CLOAK_AND_DAGGER_SUPPORTED_SUBSET,
        "Cloak and Dagger, Entwined",
        &[],
        &[],
        &[],
    );
    let etb = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ChangesZone)
        .expect("ETB trigger");
    let execute = etb.execute.as_ref().expect("trigger.execute");

    let mut cursor = Some(execute.as_ref());
    let mut found_duration = None;
    while let Some(def) = cursor {
        if let Effect::ChangeZone {
            destination: Zone::Exile,
            ..
        } = def.effect.as_ref()
        {
            found_duration = Some(def.duration.clone());
            break;
        }
        cursor = def.sub_ability.as_deref();
    }

    assert_eq!(
        found_duration,
        Some(Some(Duration::UntilHostLeavesPlay)),
        "expected the hand-exile sub-ability to carry Duration::UntilHostLeavesPlay \
         (plural 'leave the battlefield' must parse like the singular form)"
    );
}

/// CR 115.1d + CR 608.2c: the full Oracle text preserves both independently
/// announced target slots and both exile alternatives without an unsupported
/// node or a broad immediate-parent anaphor.
#[test]
fn full_card_has_exact_target_slots_reveal_binding_and_exile_union() {
    let parsed = parse_oracle_text(
        CLOAK_AND_DAGGER_FULL,
        "Cloak and Dagger, Entwined",
        &[],
        &[],
        &[],
    );

    let execute = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ChangesZone)
        .and_then(|t| t.execute.as_deref())
        .expect("ETB execute");
    assert!(matches!(
        execute.effect.as_ref(),
        Effect::TargetOnly {
            target: TargetFilter::Typed(tf)
        } if tf.controller == Some(ControllerRef::Opponent)
    ));
    assert_eq!(
        execute.multi_target,
        Some(MultiTargetSpec::exact(QuantityExpr::Fixed { value: 1 }))
    );
    let creature = execute.sub_ability.as_deref().expect("creature slot");
    assert!(matches!(
        creature.effect.as_ref(),
        Effect::TargetOnly {
            target: TargetFilter::Typed(tf)
        } if tf.type_filters.contains(&TypeFilter::Creature)
            && tf.controller == Some(ControllerRef::TargetOpponent)
    ));
    assert_eq!(
        creature.multi_target,
        Some(MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 }))
    );
    let reveal = creature.sub_ability.as_deref().expect("reveal");
    assert!(matches!(
        reveal.effect.as_ref(),
        Effect::RevealHand {
            target: TargetFilter::ParentTargetSlot { index: 0 },
            ..
        }
    ));
    let exile = reveal.sub_ability.as_deref().expect("exile");
    let Effect::ChangeZone {
        origin: None,
        destination: Zone::Exile,
        target: TargetFilter::Or { filters },
        ..
    } = exile.effect.as_ref()
    else {
        panic!("expected heterogeneous exile union, got {:?}", exile.effect);
    };
    assert_eq!(exile.duration, Some(Duration::UntilHostLeavesPlay));
    assert!(exile.optional);
    assert!(filters.iter().any(|filter| matches!(
        filter,
        TargetFilter::Typed(tf)
            if tf.controller == Some(ControllerRef::TargetOpponent)
                && tf.properties.contains(&FilterProp::InZone { zone: Zone::Hand })
    )));
    assert!(filters.iter().any(|filter| matches!(
        filter,
        TargetFilter::And { filters }
            if filters.contains(&TargetFilter::ParentTargetSlot { index: 1 })
    )));

    fn assert_no_unimplemented(def: &engine::types::ability::AbilityDefinition) {
        assert!(!matches!(def.effect.as_ref(), Effect::Unimplemented { .. }));
        if let Some(sub) = def.sub_ability.as_deref() {
            assert_no_unimplemented(sub);
        }
        if let Some(other) = def.else_ability.as_deref() {
            assert_no_unimplemented(other);
        }
    }
    assert_no_unimplemented(execute);
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

#[test]
fn full_card_can_decline_without_exiling_either_alternative() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cloak = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Cloak and Dagger, Entwined",
            2,
            2,
            CLOAK_AND_DAGGER_FULL,
        )
        .id();
    let prey = scenario.add_creature(P1, "Opponent Creature", 2, 2).id();
    let hand_card = scenario.add_card_to_hand(P1, "Opponent Spell");
    let mut runner = scenario.build();

    runner
        .cast(cloak)
        .target_player(P1)
        .target_object(prey)
        .decline_optional()
        .resolve();

    assert_eq!(zone_of(&runner, cloak), Zone::Battlefield);
    assert_eq!(zone_of(&runner, prey), Zone::Battlefield);
    assert_eq!(zone_of(&runner, hand_card), Zone::Hand);
    assert!(
        runner.state().public_revealed_cards.contains(&hand_card),
        "the chosen opponent's hand must be revealed through ParentTargetSlot 0"
    );
    assert!(runner.state().exile_links.is_empty());
}

#[test]
fn full_card_may_omit_creature_target_and_exile_only_opponents_nonland_hand_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cloak = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Cloak and Dagger, Entwined",
            2,
            2,
            CLOAK_AND_DAGGER_FULL,
        )
        .id();
    let pick = scenario.add_card_to_hand(P1, "Opponent Spell A");
    let keep = scenario.add_card_to_hand(P1, "Opponent Spell B");
    let land = scenario.add_land_to_hand(P1, "Opponent Land").id();
    let mut runner = scenario.build();

    runner
        .cast(cloak)
        .target_player(P1)
        .accept_optional()
        .effect_zone(&[pick])
        .resolve();

    assert_eq!(zone_of(&runner, pick), Zone::Exile);
    assert_eq!(zone_of(&runner, keep), Zone::Hand);
    assert_eq!(zone_of(&runner, land), Zone::Hand);
    assert!(runner.state().exile_links.iter().any(|link| {
        link.exiled_id == pick
            && link.source_id == cloak
            && link.kind
                == ExileLinkKind::UntilSourceLeaves {
                    return_zone: Zone::Hand,
                }
    }));
}

#[test]
fn full_card_can_choose_creature_and_returns_it_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cloak = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Cloak and Dagger, Entwined",
            2,
            2,
            CLOAK_AND_DAGGER_FULL,
        )
        .id();
    let prey = scenario.add_creature(P1, "Opponent Creature", 2, 2).id();
    let hand_card = scenario.add_card_to_hand(P1, "Opponent Spell");
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .id();
    let mut runner = scenario.build();

    runner
        .cast(cloak)
        .target_player(P1)
        .target_object(prey)
        .accept_optional()
        .effect_zone(&[prey])
        .resolve();
    assert_eq!(zone_of(&runner, prey), Zone::Exile);
    assert_eq!(zone_of(&runner, hand_card), Zone::Hand);
    assert!(runner.state().exile_links.iter().any(|link| {
        link.exiled_id == prey
            && link.source_id == cloak
            && link.kind
                == ExileLinkKind::UntilSourceLeaves {
                    return_zone: Zone::Battlefield,
                }
    }));

    runner.cast(destroy).target_object(cloak).resolve();
    assert_eq!(zone_of(&runner, cloak), Zone::Graveyard);
    assert_eq!(zone_of(&runner, prey), Zone::Battlefield);
    assert!(!runner
        .state()
        .exile_links
        .iter()
        .any(|link| link.exiled_id == prey));
}

#[test]
fn until_source_leaves_does_not_begin_after_source_left_event() {
    use engine::game::effects::resolve_ability_chain;
    use engine::types::ability::{DurationEvent, ResolvedAbility, TargetFilter, TypedFilter};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Departed Exiler", 2, 2).id();
    let candidate = scenario.add_card_to_hand(P1, "Opponent Spell");
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&source).unwrap().zone = Zone::Graveyard;

    let mut ability = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Hand),
            destination: Zone::Exile,
            target: TargetFilter::Typed(
                TypedFilter::card()
                    .controller(ControllerRef::Opponent)
                    .properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
            ),
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![],
        source,
        P0,
    );
    ability.duration = Some(Duration::UntilHostLeavesPlay);
    ability
        .context
        .duration_events
        .push(DurationEvent::SourceLeftBattlefield);

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("expired duration must resolve as a no-op");
    assert_eq!(zone_of(&runner, candidate), Zone::Hand);
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::EffectZoneChoice { .. }
    ));
    assert!(runner.state().exile_links.is_empty());
}

/// Runtime regression for review blocker 1: TWO eligible nonland candidates
/// force the interactive `EffectZoneChoice` round-trip (a lone candidate
/// takes the single-candidate shortcut and skips it). The chosen card's
/// "until the source leaves the battlefield" exile link must survive that
/// round-trip, and the card must return to its owner's hand when the source
/// leaves the battlefield.
///
/// The bounded exile is also driven as a hand-built `ResolvedAbility` through the
/// production `resolve_ability_chain` -> `change_zone::resolve` ->
/// `WaitingFor::EffectZoneChoice` -> `engine_resolution_choices` resume
/// pipeline — the exact low-level authority the original review named — so the
/// duration carrier remains covered independently of the full-card E2E paths.
#[test]
fn interactive_two_candidate_exile_choice_preserves_until_leaves_link() {
    use engine::game::effects::resolve_ability_chain;
    use engine::types::ability::{Effect, ResolvedAbility, TargetFilter, TypeFilter, TypedFilter};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let source = scenario.add_creature(P0, "Linked Exiler", 2, 2).id();

    // P1's hand: TWO nonland cards (both eligible -> a real interactive
    // choice) and one land (must stay excluded by the "nonland card" filter).
    let pick = scenario.add_card_to_hand(P1, "Opponent's Spell A");
    let keep = scenario.add_card_to_hand(P1, "Opponent's Spell B");
    let land_card = scenario.add_land_to_hand(P1, "Opponent's Island").id();

    let destroy_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .id();

    let mut runner = scenario.build();

    // Sequester `destroy_spell` out of P0's hand during the exile step — the
    // filter scans every hand, and a nonland card in the caster's own hand
    // would otherwise join the candidate pool.
    {
        let state = runner.state_mut();
        state.objects.get_mut(&destroy_spell).unwrap().zone = Zone::Library;
        state.players[P0.0 as usize]
            .hand
            .retain(|&id| id != destroy_spell);
        state.players[P0.0 as usize]
            .library
            .push_back(destroy_spell);
    }

    // The bounded move: "exile a nonland card [from a hand] until <source>
    // leaves the battlefield" — one pick (no `multi_target` => choice count 1)
    // with the host-lifetime duration on the resolving ability.
    let mut ability = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Hand),
            destination: Zone::Exile,
            target: TargetFilter::Typed(TypedFilter {
                type_filters: vec![
                    TypeFilter::Card,
                    TypeFilter::Non(Box::new(TypeFilter::Land)),
                ],
                ..TypedFilter::default()
            }),
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![],
        source,
        P0,
    );
    ability.duration = Some(Duration::UntilHostLeavesPlay);

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("resolving the bounded exile must succeed");

    let WaitingFor::EffectZoneChoice {
        cards,
        count,
        duration,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "two eligible candidates must raise an interactive EffectZoneChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        cards.contains(&pick) && cards.contains(&keep),
        "both nonland hand cards must be offered; got {cards:?}"
    );
    assert!(
        !cards.contains(&land_card),
        "the land must not be an eligible exile candidate"
    );
    assert_eq!(count, 1, "exactly one card is exiled");
    assert_eq!(
        duration,
        Some(Duration::UntilHostLeavesPlay),
        "review blocker 1: the bounded-move duration must be CARRIED on the \
         EffectZoneChoice round-trip, not dropped"
    );

    runner
        .act(GameAction::SelectCards { cards: vec![pick] })
        .expect("selecting one of the two candidates must succeed");

    assert_eq!(zone_of(&runner, pick), Zone::Exile, "chosen card exiled");
    assert_eq!(
        zone_of(&runner, keep),
        Zone::Hand,
        "unchosen card stays in hand"
    );
    assert!(
        runner.state().exile_links.iter().any(|link| {
            link.exiled_id == pick
                && link.source_id == source
                && link.kind
                    == ExileLinkKind::UntilSourceLeaves {
                        return_zone: Zone::Hand,
                    }
        }),
        "review blocker 1: the UntilSourceLeaves link must survive the \
         interactive EffectZoneChoice round-trip; got {:?}",
        runner.state().exile_links
    );

    // Restore the sequestered destroy spell and remove the source.
    {
        let state = runner.state_mut();
        state.objects.get_mut(&destroy_spell).unwrap().zone = Zone::Hand;
        state.players[P0.0 as usize]
            .library
            .retain(|&id| id != destroy_spell);
        state.players[P0.0 as usize].hand.push_back(destroy_spell);
    }
    runner.cast(destroy_spell).target_object(source).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, source),
        Zone::Graveyard,
        "the source should be destroyed"
    );
    assert_eq!(
        zone_of(&runner, pick),
        Zone::Hand,
        "the interactively chosen exile must RETURN when the source leaves — \
         not stay exiled forever"
    );
    assert!(
        runner.state().players[P1.0 as usize].hand.contains(&pick),
        "returned card must actually be back in P1's hand zone list"
    );
    assert!(
        !runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.exiled_id == pick),
        "the exile link must be cleared once the card has returned"
    );
}

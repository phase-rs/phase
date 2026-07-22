//! Issue #4235: Cloak and Dagger, Entwined — plural "leave the battlefield"
//! duration parsing, interactive-choice duration carry-through, and coverage
//! honesty for the card's unsupported "or the chosen creature" alternative.
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
//! 3. COVERAGE HONESTY (review blocker 2): the full printed sentence carries
//!    an "or the chosen creature" alternative that depends on the secondary
//!    "up to one target creature they control" declaration — neither of
//!    which is representable yet (no object-anaphor "the chosen creature"
//!    filter exists, and the hand filter is not bound to the chosen
//!    opponent). Rather than accept the card while silently dropping the
//!    alternative, the full clause now stays an explicit strict failure
//!    (`Effect::Unimplemented`) until that binding is implemented; the
//!    duration and carry-through fixes are exercised through the supported
//!    single-referent form of the same idiom.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Duration, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::{ExileLinkKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

/// Cloak and Dagger's full, real printed text — carries the unsupported
/// "or the chosen creature" alternative.
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

/// Coverage honesty (review blocker 2): the FULL printed text's exile clause
/// carries the "or the chosen creature" alternative, which has no
/// representable lowering yet. The clause must surface as an explicit strict
/// failure — an `Effect::Unimplemented` node somewhere in the trigger chain —
/// not as a supported-looking exile that silently dropped the alternative.
#[test]
fn full_card_with_chosen_creature_alternative_stays_strict_failure() {
    let parsed = parse_oracle_text(
        CLOAK_AND_DAGGER_FULL,
        "Cloak and Dagger, Entwined",
        &[],
        &[],
        &[],
    );

    fn chain_has_unimplemented(def: &engine::types::ability::AbilityDefinition) -> bool {
        fn effect_unimplemented(effect: &Effect) -> bool {
            matches!(effect, Effect::Unimplemented { .. })
        }
        effect_unimplemented(&def.effect)
            || def
                .sub_ability
                .as_deref()
                .is_some_and(chain_has_unimplemented)
            || def
                .else_ability
                .as_deref()
                .is_some_and(chain_has_unimplemented)
    }

    let etb_has_strict_failure = parsed
        .triggers
        .iter()
        .filter(|t| t.mode == TriggerMode::ChangesZone)
        .filter_map(|t| t.execute.as_deref())
        .any(chain_has_unimplemented);
    assert!(
        etb_has_strict_failure,
        "the unsupported 'or the chosen creature' alternative must keep the \
         clause an explicit Unimplemented strict failure, not a silently \
         narrowed exile; got triggers: {:#?}",
        parsed.triggers
    );
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// Runtime regression for review blocker 1: TWO eligible nonland candidates
/// force the interactive `EffectZoneChoice` round-trip (a lone candidate
/// takes the single-candidate shortcut and skips it). The chosen card's
/// "until the source leaves the battlefield" exile link must survive that
/// round-trip, and the card must return to its owner's hand when the source
/// leaves the battlefield.
///
/// The bounded exile is driven as a hand-built `ResolvedAbility` through the
/// production `resolve_ability_chain` -> `change_zone::resolve` ->
/// `WaitingFor::EffectZoneChoice` -> `engine_resolution_choices` resume
/// pipeline — the exact authority the review named — rather than through
/// Cloak and Dagger's printed text: the full card is now an honest strict
/// failure (see the coverage-honesty test above), so no parsed card text can
/// exercise this seam for the two-candidate case yet.
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

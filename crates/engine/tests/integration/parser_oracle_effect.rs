//! Tests relocated from `src/parser/oracle_effect/mod.rs` and its `tests.rs` test module.

use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
use engine::game::coverage::card_face_has_unimplemented_parts;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_effect::test_helpers::{boon_strings, outer_gap_names};
use engine::parser::oracle_effect::{parse_effect, parse_effect_chain};
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, ControllerRef, DelayedTriggerCondition,
    DigRestOrder, Effect, EffectOutcomeSignal, ExtraPhaseRecipient, FilterProp, QuantityExpr,
    QuantityRef, RevealUntilDisposition, RuntimeHandler, TargetChoiceTiming, TargetFilter,
    TargetRef, TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::{Phase, PhaseGroup, TurnSegment};
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

#[test]
fn synthetic_isolated_production_grants_preserve_bound_fields_and_targets() {
    let state = GameScenario::new().build();
    let phase_anchor = engine::types::ability::ExtraPhaseAnchor::ThisPhase { named: None };
    let step_anchor = engine::types::ability::ExtraPhaseAnchor::ThisStep;
    for (text, recipient, count, segment, after, legal_targets) in [
        (
            "You get an additional upkeep step after this phase.",
            ExtraPhaseRecipient::Controller,
            QuantityExpr::Fixed { value: 1 },
            TurnSegment::CreatedPhase(Phase::Upkeep),
            phase_anchor.clone(),
            vec![],
        ),
        (
            "You get an additional combat phase after this phase.",
            ExtraPhaseRecipient::Controller,
            QuantityExpr::Fixed { value: 1 },
            TurnSegment::Phase(PhaseGroup::Combat),
            phase_anchor.clone(),
            vec![],
        ),
        (
            "There is an additional upkeep step after this step.",
            ExtraPhaseRecipient::NoPlayer,
            QuantityExpr::Fixed { value: 1 },
            TurnSegment::Step(Phase::Upkeep),
            step_anchor.clone(),
            vec![],
        ),
        (
            "After this phase, there are three additional combat phases.",
            ExtraPhaseRecipient::NoPlayer,
            QuantityExpr::Fixed { value: 3 },
            TurnSegment::Phase(PhaseGroup::Combat),
            phase_anchor.clone(),
            vec![],
        ),
        (
            "After this step, you get two additional upkeep steps.",
            ExtraPhaseRecipient::Controller,
            QuantityExpr::Fixed { value: 2 },
            TurnSegment::Step(Phase::Upkeep),
            step_anchor,
            vec![],
        ),
        (
            "You get that many additional upkeep steps after this phase.",
            ExtraPhaseRecipient::Controller,
            QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            },
            TurnSegment::CreatedPhase(Phase::Upkeep),
            phase_anchor.clone(),
            vec![],
        ),
        (
            "Target player gets an additional combat phase after this phase.",
            ExtraPhaseRecipient::TargetedPlayer(TargetFilter::Player),
            QuantityExpr::Fixed { value: 1 },
            TurnSegment::Phase(PhaseGroup::Combat),
            phase_anchor.clone(),
            vec![TargetRef::Player(P0), TargetRef::Player(P1)],
        ),
        (
            "Target opponent gets an additional combat phase after this phase.",
            ExtraPhaseRecipient::TargetedPlayer(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent),
            )),
            QuantityExpr::Fixed { value: 1 },
            TurnSegment::Phase(PhaseGroup::Combat),
            phase_anchor,
            vec![TargetRef::Player(P1)],
        ),
    ] {
        let parsed = parse_oracle_text(text, "Synthetic Probe", &[], &["Sorcery".into()], &[]);
        assert_eq!(parsed.abilities.len(), 1, "{text:?}");
        let def = &parsed.abilities[0];
        assert_eq!(
            *def.effect,
            Effect::AdditionalPhase {
                recipient,
                count,
                segment,
                after,
                followed_by: vec![],
                attacker_restriction: None,
            },
            "{text:?}"
        );
        assert!(
            def.sub_ability.is_none() && def.else_ability.is_none(),
            "{text:?}"
        );
        let resolved = build_resolved_from_def(def, ObjectId(0), P0);
        let slots = build_target_slots(state.state(), &resolved).expect("target slots build");
        if legal_targets.is_empty() {
            assert!(slots.is_empty(), "{text:?}");
        } else {
            assert_eq!(slots.len(), 1, "{text:?}");
            assert!(!slots[0].optional, "{text:?}");
            assert_eq!(slots[0].legal_targets, legal_targets, "{text:?}");
        }
    }
}

/// A clause harness drives the delayed normalization branch and the actual
/// resolution-time choice, independently of the immediate Technomancer ETB.
#[test]
fn delayed_aggregate_return_rejects_excess_count_then_returns_selected_cards() {
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;
    use engine::types::phase::Phase;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ids: Vec<_> = (0..3)
        .map(|_| {
            scenario
                .add_creature_to_graveyard(P0, "Memnite", 1, 1)
                .as_artifact_creature()
                .with_mana_cost(ManaCost::zero())
                .id()
        })
        .collect();
    let spell = scenario.add_spell_to_hand_from_oracle(
        P0, "Delayed return clause harness", false,
        "At the beginning of the next end step, return up to two creature cards with total mana value 4 or less from your graveyard to the battlefield.",
    ).with_mana_cost(ManaCost::zero()).id();
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    assert_eq!(runner.state().delayed_triggers.len(), 1);
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::ChooseFromZoneChoice { cards, count: 2, .. }
        if ids.iter().all(|id| cards.contains(id)))
    );
    let prompt = runner.state().waiting_for.clone();
    assert!(runner
        .act(GameAction::SelectCards { cards: ids.clone() })
        .is_err());
    assert_eq!(runner.state().waiting_for, prompt);
    assert!(ids
        .iter()
        .all(|id| runner.state().objects[id].zone == Zone::Graveyard));
    runner
        .act(GameAction::SelectCards {
            cards: ids[..2].to_vec(),
        })
        .unwrap();
    assert_eq!(runner.state().objects[&ids[0]].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&ids[1]].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&ids[2]].zone, Zone::Graveyard);
}

#[test]
fn relic_of_progenitus_target_player_exiles_from_their_graveyard() {
    // GitHub phase-rs/phase#6446 (supersedes #1077 Stack+TargetPlayer framing):
    // "Target player exiles a card from their graveyard." The ability targets
    // ONLY the player (CR 115.1c). The card is an untargeted resolution choice
    // by that player (CR 115.10a + CR 608.2d) — `Owned{ScopedPlayer}` +
    // `TargetChoiceTiming::Resolution` — so companion TargetPlayer and card
    // object stack slots do not appear (exactly one target slot).
    // Same class: Scrabbling Claws, Merrow Bonegnawer, Graveyard Shovel,
    // Grave Birthing, Gravestorm. Contrast Memory's Journey ("target cards
    // from their graveyard") which stays Stack + Owned{TargetPlayer}.
    let def = parse_effect_chain(
        "Target player exiles a card from their graveyard.",
        AbilityKind::Activated,
    );
    let Effect::TargetOnly {
        target: player_target,
    } = &*def.effect
    else {
        panic!("expected TargetOnly player wrapper, got {:?}", def.effect);
    };
    assert_eq!(
        *player_target,
        TargetFilter::Player,
        "the wrapper's declared target must be the player, got {player_target:?}"
    );
    let sub = def
        .sub_ability
        .as_ref()
        .expect("exile effect should be in sub-ability after the player target");
    assert_eq!(
        sub.target_choice_timing,
        TargetChoiceTiming::Resolution,
        "GY card is chosen at resolution, not announced as a stack target"
    );
    let Effect::ChangeZone {
        origin,
        destination,
        target,
        ..
    } = sub.effect.as_ref()
    else {
        panic!(
            "expected ChangeZone exile in sub-ability, got {:?}",
            sub.effect
        );
    };
    assert_eq!(
        *origin,
        Some(Zone::Graveyard),
        "must exile from graveyard, not an open-zone pick"
    );
    assert_eq!(*destination, Zone::Exile);
    let TargetFilter::Typed(typed) = target else {
        panic!("expected typed card target, got {target:?}");
    };
    assert!(
        typed.properties.contains(&FilterProp::InZone {
            zone: Zone::Graveyard
        }),
        "target must be constrained to graveyard, got {typed:?}"
    );
    assert!(
        typed.properties.contains(&FilterProp::Owned {
            controller: ControllerRef::ScopedPlayer
        }),
        "must keep ScopedPlayer so the targeted player chooses at resolution — got {typed:?}"
    );
    assert!(
        !typed.properties.contains(&FilterProp::Owned {
            controller: ControllerRef::TargetPlayer
        }),
        "must not rebind to TargetPlayer (that forced activator stack targeting) — got {typed:?}"
    );

    // Exactly one stack slot (the player). Revert Resolution stamp or
    // TargetPlayer rebind → companion + card slots → len 2 or 3.
    let state = engine::types::game_state::GameState::new_two_player(42);
    let resolved = engine::game::ability_utils::build_resolved_from_def(
        &def,
        engine::types::identifiers::ObjectId(1),
        engine::types::player::PlayerId(0),
    );
    let slots = engine::game::ability_utils::build_target_slots(&state, &resolved)
        .expect("Relic first ability must build target slots");
    assert_eq!(
        slots.len(),
        1,
        "exactly one stack target (the player); got {slots:?}"
    );
}

/// Discriminating runtime test: parse the real clause and resolve its Draw
/// count through the live quantity resolver against recorded discard state.
/// If the parser fix is reverted, the count is `Fixed(1)` and resolves to 1
/// regardless of discards, so the `resolved == 3` assertion flips and fails.
#[test]
fn for_each_cards_discarded_this_turn_resolves_dynamic_draw_count() {
    use engine::game::quantity::resolve_quantity;
    use engine::game::restrictions::record_discard;
    use engine::game::zones::create_object;
    use engine::types::game_state::GameState;
    use engine::types::identifiers::CardId;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    let count = match parse_effect("draw a card for each card you've discarded this turn") {
        Effect::Draw { count, .. } => count,
        other => panic!("expected Draw, got {other:?}"),
    };

    let mut state = GameState::new_two_player(42);
    let controller = PlayerId(0);
    let source = create_object(
        &mut state,
        CardId(1),
        controller,
        "Misty Knight, Hero for Hire".to_string(),
        Zone::Battlefield,
    );

    // Controller has discarded three cards this turn.
    record_discard(&mut state, controller);
    record_discard(&mut state, controller);
    record_discard(&mut state, controller);

    let resolved = resolve_quantity(&state, &count, controller, source);
    assert_eq!(
        resolved, 3,
        "draw count must resolve to the controller's discard tally (3), not Fixed(1)"
    );
}

/// Parse a full card's Oracle text and assert (reach guard) that no part of it
/// lowered to `Effect::Unimplemented` — including payloads nested in delayed
/// or triggered abilities.
fn parse_card_fully(name: &str, card_type: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &[card_type.to_string()], &[]);
    let face = CardFace {
        name: name.to_string(),
        oracle_text: Some(oracle.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        additional_cost: parsed.additional_cost,
        ..Default::default()
    };
    assert!(
        !card_face_has_unimplemented_parts(&face),
        "{name} must parse with no Unimplemented part: {face:#?}"
    );
    face
}

/// Assert the reveal-until "that many creature cards" → matched set onto the
/// battlefield → shuffle chain shared by Mass Polymorph and Synthetic Destiny.
fn assert_that_many_creatures_onto_battlefield_then_shuffle(reveal: &AbilityDefinition) {
    let Effect::RevealUntil {
        player,
        filter,
        count,
        matched_disposition,
        kept_destination,
        rest_destination,
        ..
    } = &*reveal.effect
    else {
        panic!("expected RevealUntil, got {:?}", reveal.effect);
    };
    assert_eq!(*player, TargetFilter::Controller);
    assert_eq!(*filter, TargetFilter::Typed(TypedFilter::creature()));
    assert_eq!(
        *count,
        QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount
        },
        "\"that many\" is the preceding instruction's count"
    );
    assert_eq!(*matched_disposition, RevealUntilDisposition::KeepEach);
    assert_eq!(
        *kept_destination,
        Zone::Battlefield,
        "\"put all creature cards revealed this way onto the battlefield\" is the matched set"
    );
    assert_eq!(*rest_destination, Zone::Library);
    let shuffle = reveal
        .sub_ability
        .as_deref()
        .expect("RevealUntil must chain into the shuffle");
    assert_eq!(
        *shuffle.effect,
        Effect::Shuffle {
            target: TargetFilter::Controller
        },
        "the matched set is absorbed (no ChangeZoneAll sibling); the shuffle follows"
    );
    assert!(shuffle.sub_ability.is_none());
}

/// CR 608.2c + CR 608.2h + CR 701.20a: Mass Polymorph (verbatim) — the count
/// slot "that many" delegates to the demonstrative-amount authority, and "put
/// all creature cards revealed this way onto the battlefield" names the matched
/// set, so the RevealUntil keeps its hits on the battlefield.
#[test]
fn reveal_until_that_many_mass_polymorph() {
    let face = parse_card_fully(
        "Mass Polymorph",
        "Sorcery",
        "Exile all creatures you control, then reveal cards from the top of your library until you reveal that many creature cards. Put all creature cards revealed this way onto the battlefield, then shuffle the rest of the revealed cards into your library.",
    );
    let root = face.abilities.first().expect("spell ability");
    let Effect::ChangeZoneAll {
        destination,
        target,
        ..
    } = &*root.effect
    else {
        panic!("expected ChangeZoneAll root, got {:?}", root.effect);
    };
    assert_eq!(*destination, Zone::Exile);
    assert_eq!(
        *target,
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
    );
    let reveal = root
        .sub_ability
        .as_deref()
        .expect("exile must chain into RevealUntil");
    assert_that_many_creatures_onto_battlefield_then_shuffle(reveal);
}

/// CR 603.7a + CR 608.2c: Synthetic Destiny (verbatim) — the same reveal chain
/// inside the end-step delayed trigger created by the spell.
#[test]
fn reveal_until_that_many_synthetic_destiny() {
    let face = parse_card_fully(
        "Synthetic Destiny",
        "Instant",
        "Exile all creatures you control. At the beginning of the next end step, reveal cards from the top of your library until you reveal that many creature cards, put all creature cards revealed this way onto the battlefield, then shuffle the rest of the revealed cards into your library.",
    );
    let root = face.abilities.first().expect("spell ability");
    assert!(
        matches!(
            &*root.effect,
            Effect::ChangeZoneAll {
                destination: Zone::Exile,
                ..
            }
        ),
        "expected the exile root, got {:?}",
        root.effect
    );
    let delayed = root
        .sub_ability
        .as_deref()
        .expect("exile must chain into the delayed trigger");
    let Effect::CreateDelayedTrigger {
        condition, effect, ..
    } = &*delayed.effect
    else {
        panic!("expected CreateDelayedTrigger, got {:?}", delayed.effect);
    };
    assert_eq!(
        *condition,
        DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    );
    assert_that_many_creatures_onto_battlefield_then_shuffle(effect);
}

/// CR 701.20a + CR 608.2c: Old Stickfingers (full verbatim text) — "Put all
/// creature cards revealed this way into your graveyard" is the matched set's
/// disposition: the kept zone is the graveyard (not the default hand plus a
/// stray ChangeZoneAll), and the rest go to the bottom in a random order.
#[test]
fn reveal_until_x_matched_set_into_graveyard_old_stickfingers() {
    let face = parse_card_fully(
        "Old Stickfingers",
        "Creature",
        "When you cast this spell, reveal cards from the top of your library until you reveal X creature cards. Put all creature cards revealed this way into your graveyard, then put the rest on the bottom of your library in a random order.\nOld Stickfingers's power and toughness are each equal to the number of creature cards in your graveyard.",
    );
    let trigger = face.triggers.first().expect("cast trigger");
    let execute = trigger.execute.as_deref().expect("trigger body");
    let Effect::RevealUntil {
        count,
        filter,
        kept_destination,
        rest_destination,
        rest_order,
        ..
    } = &*execute.effect
    else {
        panic!("expected RevealUntil, got {:?}", execute.effect);
    };
    assert_eq!(
        *count,
        QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string()
            }
        }
    );
    assert_eq!(*filter, TargetFilter::Typed(TypedFilter::creature()));
    assert_eq!(*kept_destination, Zone::Graveyard);
    assert_eq!(*rest_destination, Zone::Library);
    assert_eq!(*rest_order, DigRestOrder::Random);
    assert!(
        execute.sub_ability.is_none(),
        "the matched set is absorbed — no ChangeZoneAll sibling: {:?}",
        execute.sub_ability
    );
}

/// CR 608.2c + CR 608.2d: the precedence guard at `oracle_trigger.rs`
/// exists deterministically. Routed through `crate::source_census::code` (the
/// path-qualified marker `no_source_reading_file_carries_a_private_comment_policy`
/// looks for — a bare `source_census` import is a measured FALSE PASS).
/// `subject_anchored_optional_actor` (`assembly.rs`) is the authority that wins;
/// this file's detector only fills in where that authority declined
/// (`ability.optional_player.is_none()`).
#[test]
fn the_they_may_detector_write_is_guarded_by_an_is_none_precedence_check() {
    let text = include_str!("../../../phase-parser/src/parser/oracle_trigger.rs");
    let lines: Vec<&str> = text.lines().collect();
    let needle = "ability.optional_player = Some(";
    let guard = "optional_player.is_none()";
    let mut assign_count = 0usize;
    let mut guarded_count = 0usize;
    for (i, raw) in lines.iter().enumerate() {
        if crate::source_census::code(raw).contains(needle) {
            assign_count += 1;
            let window_start = i.saturating_sub(10);
            let is_guarded = lines[window_start..=i]
                .iter()
                .any(|l| crate::source_census::code(l).contains(guard));
            if is_guarded {
                guarded_count += 1;
            }
        }
    }
    assert_eq!(
        assign_count, 1,
        "positive control: the write must exist exactly once in oracle_trigger.rs"
    );
    assert_eq!(
        guarded_count, assign_count,
        "every `ability.optional_player = Some(` assignment in oracle_trigger.rs must be preceded, \
         within its enclosing block, by an `optional_player.is_none()` precedence guard"
    );
}

/// The new wire tags round-trip, and the new transient state slot serializes only
/// while set (every serialized state is byte-identical while it is false).
#[test]
fn repeat_directive_wire_shapes_round_trip() {
    let effect = Effect::RuntimeHandled {
        handler: RuntimeHandler::RepeatProcess,
    };
    let json = serde_json::to_string(&effect).unwrap();
    assert_eq!(serde_json::from_str::<Effect>(&json).unwrap(), effect);
    let signal = AbilityCondition::EffectOutcome {
        signal: EffectOutcomeSignal::RepeatDirectiveReached,
    };
    let json = serde_json::to_string(&signal).unwrap();
    assert_eq!(
        serde_json::from_str::<AbilityCondition>(&json).unwrap(),
        signal
    );

    let mut state = engine::types::game_state::GameState::new_two_player(42);
    let value = serde_json::to_value(&state).unwrap();
    assert!(
        value.get("repeat_process_requested").is_none(),
        "a default state omits the slot"
    );
    let request = engine::types::game_state::RepeatProcessRequest {
        source_id: engine::types::identifiers::ObjectId(7),
    };
    state.repeat_process_requested = Some(request);
    let mut value = serde_json::to_value(&state).unwrap();
    assert_eq!(
        value["repeat_process_requested"],
        serde_json::json!({ "source_id": 7 })
    );
    let round_tripped: engine::types::game_state::GameState =
        serde_json::from_value(value.clone()).unwrap();
    assert_eq!(round_tripped.repeat_process_requested, Some(request));
    value
        .as_object_mut()
        .unwrap()
        .remove("repeat_process_requested");
    let restored: engine::types::game_state::GameState = serde_json::from_value(value).unwrap();
    assert_eq!(
        restored.repeat_process_requested, None,
        "a payload without it reads as no request"
    );
}

/// SHAPE + coverage honesty: unsupported target roles survive the complete
/// top-level router instead of becoming broad prevention replacements.
#[test]
fn prevention_target_role_gaps_survive_top_level_fallback() {
    let fixtures: &[(&str, &str, &[&str], &[&str])] = &[
        ("Resistance Fighter", "Sacrifice this creature: Prevent all combat damage target creature would deal this turn.", &["Creature"], &["Human", "Soldier"]),
        ("Falling Timber", "Kicker—Sacrifice a land. (You may sacrifice a land in addition to any other costs as you cast this spell.)\nPrevent all combat damage target creature would deal this turn. If this spell was kicked, prevent all combat damage another target creature would deal this turn.", &["Instant"], &[]),
        ("Chain of Silence", "Prevent all damage target creature would deal this turn. That creature's controller may sacrifice a land of their choice. If the player does, they may copy this spell and may choose a new target for that copy.", &["Instant"], &[]),
        ("Serene Sunset", "Prevent all combat damage X target creatures would deal this turn.", &["Instant"], &[]),
        ("Soul Parry", "Prevent all damage one or two target creatures would deal this turn.", &["Instant"], &[]),
        ("Guard Dogs", "{2}{W}, {T}: Choose a permanent you control. Prevent all combat damage target creature would deal this turn if it shares a color with that permanent.", &["Creature"], &["Dog"]),
        ("Azorius Ploy", "Prevent all combat damage target creature would deal this turn.\nPrevent all combat damage that would be dealt to target creature this turn.", &["Instant"], &[]),
        ("Gideon of the Trials", "[+1]: Until your next turn, prevent all damage target permanent would deal.\n[0]: Until end of turn, Gideon becomes a 4/4 Human Soldier creature with indestructible that's still a planeswalker. Prevent all damage that would be dealt to him this turn.\n[0]: You get an emblem with \"As long as you control a Gideon planeswalker, you can't lose the game and your opponents can't win the game.\"", &["Planeswalker"], &["Gideon"]),
        ("Shieldmage Elder", "Tap two untapped Clerics you control: Prevent all damage target creature would deal this turn.\nTap two untapped Wizards you control: Prevent all damage target spell would deal this turn.", &["Creature"], &["Human", "Cleric", "Wizard"]),
        ("Stonewise Fortifier", "{4}{W}: Prevent all damage that would be dealt to this creature by target creature this turn.", &["Creature"], &["Human", "Wizard"]),
        ("Terrifying Presence", "Prevent all combat damage that would be dealt by creatures other than target creature this turn.", &["Instant"], &[]),
        ("Wojek Apothecary", "Radiance — {T}: Prevent the next 1 damage that would be dealt to target creature and each other creature that shares a color with it this turn.", &["Creature"], &["Human", "Cleric"]),
        ("Encircling Fissure", "Prevent all combat damage that would be dealt this turn by creatures target opponent controls.\nAwaken 2—{4}{W} (If you cast this spell for {4}{W}, also put two +1/+1 counters on target land you control and it becomes a 0/0 Elemental creature with haste. It's still a land.)", &["Instant"], &[]),
        ("Inquisitor's Snare", "Prevent all damage target attacking or blocking creature would deal this turn. If that creature is black or red, destroy it.", &["Instant"], &[]),
        ("Old Fat Spider Can't See Me", "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after IV.)\nI — Target creature you control gains hexproof for as long as this Saga remains on the battlefield.\nII — Prevent all damage that would be dealt by up to one target creature for as long as this Saga remains on the battlefield.\nIII, IV — Draw a card.", &["Enchantment"], &["Saga"]),
    ];
    for &(name, oracle, types, subtypes) in fixtures {
        let types = types.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let subtypes = subtypes.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let parsed = parse_oracle_text(oracle, name, &[], &types, &subtypes);
        assert!(
            !parsed.abilities.is_empty() || !parsed.triggers.is_empty(),
            "{name}: reach guard: the complete card produced executable definitions"
        );
        assert!(
            parsed
                .replacements
                .iter()
                .all(|replacement| replacement.event != ReplacementEvent::DamageDone),
            "{name}: a rejected target role must not become a broad replacement: {:?}",
            parsed.replacements
        );
        let mut has_target_role_gap = false;
        let mut visit = |effect: &Effect| {
            has_target_role_gap |= matches!(
                effect,
                Effect::Unimplemented { name, .. }
                    if name == "prevent_damage_recipient_target_role"
            );
            std::ops::ControlFlow::Continue(())
        };
        for ability in &parsed.abilities {
            let _ = engine::types::ability_visit::visit_ability_def(ability, &mut visit);
        }
        for trigger in &parsed.triggers {
            let _ = engine::types::ability_visit::visit_trigger(trigger, &mut visit);
        }
        assert!(
            has_target_role_gap,
            "{name}: the rejected prevention role must retain its specific gap"
        );
        let face = CardFace {
            name: name.to_string(),
            oracle_text: Some(oracle.to_string()),
            abilities: parsed.abilities,
            triggers: parsed.triggers,
            static_abilities: parsed.statics,
            replacements: parsed.replacements,
            additional_cost: parsed.additional_cost,
            ..Default::default()
        };
        assert!(
            card_face_has_unimplemented_parts(&face),
            "{name}: strict unsupported coverage must survive the top-level router"
        );
    }
}

/// B2/U3d: a free batch play with NO stated duration (Vault 112) has no
/// window to share, so it refuses honestly.
#[test]
fn free_one_of_batch_grants_refuse_honestly() {
    // Vault 112 only (single shape — no loop: the Ozai free+stated sibling
    // promotes per U3d, so it no longer belongs in this refusal pin).
    let card = "Vault 112";
    let text = "III — Pay any amount of {E}. If you paid one or more {E} this way, shuffle your library, then exile that many cards from the top. You may play one of those cards without paying its mana cost.";
    let (keywords, types, subtypes): (&[&str], &[&str], &[&str]) =
        (&[], &["Enchantment"], &["Saga"]);
    let parsed = parse_oracle_text(
        text,
        card,
        &boon_strings(keywords),
        &boon_strings(types),
        &boon_strings(subtypes),
    );
    let gaps = outer_gap_names(&parsed);
    assert!(
        gaps.iter().any(|gap| gap == "bounded_batch_cast"),
        "{card}: the free batch play must refuse as bounded_batch_cast, got {gaps:?}: {parsed:#?}"
    );
    let face = CardFace {
        name: card.to_string(),
        oracle_text: Some(text.to_string()),
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        additional_cost: parsed.additional_cost,
        ..Default::default()
    };
    assert!(
        card_face_has_unimplemented_parts(&face),
        "{card}: must stay red: {face:#?}"
    );
}

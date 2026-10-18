//! Public U5 seek consumers. The exact first-instruction excerpt is explicitly
//! partial Oracle text; full Circadian retains its separate perpetual-grant gap.

use engine::ai_support::legal_actions;
use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    CardTypeSetSource, Effect, EffectKind, FilterProp, QuantityExpr, QuantityRef, SharedQuality,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SEEK_EXCERPT: &str = "Seek X cards that each share a color with one or more permanents you control, where X is the number of colors among permanents you control.";
const FULL_CIRCADIAN: &str = "Vivid — Seek X cards that each share a color with one or more permanents you control, where X is the number of colors among permanents you control. For each color among permanents you control, those cards perpetually gain \"This spell costs {1} less to cast.\"";
const THOUGHTLACE: &str =
    "Target spell or permanent becomes blue. (Mana symbols on that permanent remain unchanged.)";
const DISPERSE: &str = "Return target nonland permanent to its owner's hand.";
const ANTHEM: &str = "Creatures you control get +1/+1.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const LEYLINE: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nIf a card would be put into an opponent's graveyard from anywhere, exile it instead.";

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_mana_pool(
            player,
            [
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
            ]
            .into_iter()
            .flat_map(|kind| (0..20).map(move |_| ManaUnit::new(kind, ObjectId(0), false, vec![])))
            .collect(),
        );
    }
    scenario
}

fn seek_spell(scenario: &mut GameScenario, player: PlayerId, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, "Explicit U5 instruction excerpt", true, text)
        .with_mana_cost(ManaCost::generic(1))
        .id()
}

fn library_card(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    colors: &[ManaColor],
) -> ObjectId {
    scenario
        .add_spell_to_library_top(player, name, true)
        .with_color(colors.to_vec())
        .id()
}

fn assert_bound_seek(text: &str, full: bool) {
    let parsed = parse_oracle_text(text, "Circadian Struggle", &[], &["Instant".into()], &[]);
    assert_eq!(parsed.abilities.len(), 1, "{parsed:#?}");
    assert!(
        parsed.statics.is_empty(),
        "quoted grant cannot become a host cost static"
    );
    let Effect::Seek {
        filter: TargetFilter::Typed(typed),
        count,
        ..
    } = parsed.abilities[0].effect.as_ref()
    else {
        panic!("{parsed:#?}")
    };
    let QuantityExpr::Ref {
        qty:
            QuantityRef::DistinctColorsAmong {
                source: CardTypeSetSource::Objects { filter: population },
            },
    } = count
    else {
        panic!("text-defined X: {count:?}")
    };
    assert!(typed.properties.iter().any(|prop| matches!(prop, FilterProp::SharesQuality { quality: SharedQuality::Color, reference: Some(reference), .. } if reference.as_ref() == population)));
    let gaps: Vec<_> =
        std::iter::successors(Some(&parsed.abilities[0]), |d| d.sub_ability.as_deref())
            .filter_map(|d| match d.effect.as_ref() {
                Effect::Unimplemented { description, .. } => {
                    Some(description.as_deref().unwrap_or(""))
                }
                _ => None,
            })
            .collect();
    if full {
        assert!(gaps.iter().any(|gap| gap
            .contains("those cards perpetually gain \"This spell costs {1} less to cast.\"")));
        assert!(gaps.iter().all(|gap| !gap.contains("Seek X cards")));
    } else {
        assert!(gaps.is_empty(), "excerpt must fully execute: {parsed:#?}");
        assert!(parsed.parse_warnings.is_empty(), "{parsed:#?}");
    }
}

fn assert_done(outcome: &CastOutcome, source: ObjectId) {
    assert!(outcome.state().stack.is_empty());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.events().iter().any(|e| matches!(e, GameEvent::EffectResolved { kind: EffectKind::Seek, source_id, .. } if *source_id == source)));
    assert!(!outcome.events().iter().any(|e| matches!(
        e,
        GameEvent::CardDrawn { .. }
            | GameEvent::EffectResolved {
                kind: EffectKind::Shuffle,
                ..
            }
    )));
}

#[test]
fn jev_dynamic_seek_shared_colors_public() {
    assert_bound_seek(SEEK_EXCERPT, false);
    let mut scenario = scenario();
    scenario
        .add_enchantment_from_oracle(P0, "White blue reference", "")
        .with_color(vec![ManaColor::White, ManaColor::Blue]);
    for name in ["Repeated white A", "Repeated white B"] {
        scenario
            .add_enchantment_from_oracle(P0, name, "")
            .with_color(vec![ManaColor::White]);
    }
    scenario.add_artifact_from_oracle(P0, "Colorless reference", "");
    let eligible = [
        library_card(&mut scenario, P0, "White", &[ManaColor::White]),
        library_card(&mut scenario, P0, "Blue", &[ManaColor::Blue]),
        library_card(
            &mut scenario,
            P0,
            "White blue",
            &[ManaColor::White, ManaColor::Blue],
        ),
        library_card(
            &mut scenario,
            P0,
            "White red",
            &[ManaColor::White, ManaColor::Red],
        ),
    ];
    let rejected = [
        library_card(&mut scenario, P0, "Colorless", &[]),
        library_card(&mut scenario, P0, "Off color", &[ManaColor::Black]),
    ];
    let spell = seek_spell(&mut scenario, P0, SEEK_EXCERPT);
    let mut runner = scenario.build();
    let before: Vec<_> = runner.state().library_of(P0).iter().copied().collect();
    // CR 105.1 + CR 107.3c: two distinct colors, with no announced-X choice.
    let outcome = runner.cast(spell).resolve();
    outcome.assert_hand_drawn(P0, 2);
    let picked: Vec<_> = eligible
        .into_iter()
        .filter(|id| outcome.zone_of(*id) == Zone::Hand)
        .collect();
    assert_eq!(picked.len(), 2);
    outcome.assert_zone(&rejected, Zone::Library);
    let expected_remaining: Vec<_> = before
        .into_iter()
        .filter(|id| !picked.contains(id))
        .collect();
    assert_eq!(
        outcome
            .state()
            .library_of(P0)
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        expected_remaining,
        "seek does not shuffle"
    );
    assert_eq!(outcome.events().iter().filter(|e| matches!(e, GameEvent::ZoneChanged { object_id, from: Some(Zone::Library), to: Zone::Hand, .. } if picked.contains(object_id))).count(), 2);
    assert_done(&outcome, spell);
}

#[test]
fn jev_circadian_u5_full_text_partial() {
    assert_bound_seek(FULL_CIRCADIAN, true);
    let mut scenario = scenario();
    scenario
        .add_enchantment_from_oracle(P0, "White blue reference", "")
        .with_color(vec![ManaColor::White, ManaColor::Blue]);
    let cards = [
        library_card(&mut scenario, P0, "White", &[ManaColor::White]),
        library_card(&mut scenario, P0, "Blue", &[ManaColor::Blue]),
    ];
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Circadian Struggle", true, FULL_CIRCADIAN)
        .with_mana_cost("{4}{G/U}{G/U}".parse().unwrap())
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    outcome.assert_hand_drawn(P0, 2);
    outcome.assert_zone(&cards, Zone::Hand);
    assert_done(&outcome, spell);
    // Only the first instruction is claimed here. The complete printed grant
    // remains strict-failed until its exact sought-set consumer lands in phase 8.
    assert!(outcome
        .state()
        .unimplemented_oracle_ids
        .contains("Circadian Struggle"));
}

#[test]
fn jev_shared_seek_reference_scope_and_relation() {
    assert_bound_seek(SEEK_EXCERPT, false);
    for player in [P0, P1] {
        let other = if player == P0 { P1 } else { P0 };
        let mut scenario = scenario();
        scenario
            .add_enchantment_from_oracle(player, "Controlled white", "")
            .with_color(vec![ManaColor::White]);
        scenario
            .add_enchantment_from_oracle(player, "Owned but opponent controlled blue", "")
            .with_color(vec![ManaColor::Blue])
            .controlled_by(other);
        scenario
            .add_enchantment_from_oracle(other, "Opponent blue", "")
            .with_color(vec![ManaColor::Blue]);
        scenario
            .add_spell_to_hand(player, "Blue hand", true)
            .with_color(vec![ManaColor::Blue]);
        scenario
            .add_spell_to_graveyard(player, "Blue graveyard", true)
            .with_color(vec![ManaColor::Blue]);
        let white = library_card(&mut scenario, player, "White eligible", &[ManaColor::White]);
        let blue = library_card(
            &mut scenario,
            player,
            "Blue library cannot reference itself",
            &[ManaColor::Blue],
        );
        let spell = seek_spell(&mut scenario, player, SEEK_EXCERPT);
        let mut runner = scenario.build();
        if player == P1 {
            runner.act(GameAction::PassPriority).unwrap();
        }
        let outcome = runner.cast(spell).resolve();
        // CR 109.2 + CR 109.5: battlefield/controller scope, never ownership or all zones.
        outcome.assert_hand_drawn(player, 1);
        outcome.assert_zone(&[white], Zone::Hand);
        outcome.assert_zone(&[blue], Zone::Library);
        assert_done(&outcome, spell);
    }
    // Explicit synthetic quality/reference sibling: no color-specific dispatch.
    let mut scenario = scenario();
    scenario
        .add_creature(P0, "Elf reference", 2, 2)
        .with_subtypes(vec!["Elf"]);
    let elf = scenario
        .add_spell_to_library_top(P0, "Elf card", false)
        .as_creature()
        .with_subtypes(vec!["Elf"])
        .id();
    let goblin = scenario
        .add_spell_to_library_top(P0, "Goblin card", false)
        .as_creature()
        .with_subtypes(vec!["Goblin"])
        .id();
    let spell = seek_spell(&mut scenario, P0, "Seek a creature card that each shares a creature type with one or more creatures you control.");
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Elf".into(), "Goblin".into()];
    let outcome = runner.cast(spell).resolve();
    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_zone(&[elf], Zone::Hand);
    outcome.assert_zone(&[goblin], Zone::Library);
    assert_done(&outcome, spell);
}

#[test]
fn jev_dynamic_seek_empty_colorless_and_insufficient() {
    assert_bound_seek(SEEK_EXCERPT, false);
    // First row is the same-harness positive reach guard for every zero row.
    for (colors, library, expected) in [
        (
            Some(vec![ManaColor::White]),
            vec![vec![ManaColor::White]],
            1,
        ),
        (None, vec![vec![ManaColor::White]], 0),
        (Some(vec![]), vec![vec![]], 0),
        (
            Some(vec![ManaColor::White]),
            vec![vec![], vec![ManaColor::Blue]],
            0,
        ),
        (
            Some(vec![ManaColor::White, ManaColor::Blue, ManaColor::Red]),
            vec![vec![ManaColor::White]],
            1,
        ),
        (Some(vec![ManaColor::White]), vec![], 0),
    ] {
        let mut scenario = scenario();
        if let Some(colors) = colors {
            scenario
                .add_enchantment_from_oracle(P0, "Reference", "")
                .with_color(colors);
        }
        let cards: Vec<_> = library
            .iter()
            .enumerate()
            .map(|(index, colors)| {
                library_card(&mut scenario, P0, &format!("Candidate {index}"), colors)
            })
            .collect();
        let spell = seek_spell(&mut scenario, P0, SEEK_EXCERPT);
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).resolve();
        outcome.assert_hand_drawn(P0, expected);
        assert_eq!(
            cards
                .iter()
                .filter(|id| outcome.zone_of(**id) == Zone::Hand)
                .count() as i64,
            expected
        );
        assert_done(&outcome, spell);
    }
}

#[test]
fn jev_seek_uses_resolution_board_and_live_reentered_noncreature() {
    assert_bound_seek(SEEK_EXCERPT, false);
    let mut scenario = scenario();
    scenario
        .add_enchantment_from_oracle(P0, "Separate blue permanent", "")
        .with_color(vec![ManaColor::Blue]);
    let anthem = scenario
        .add_spell_to_hand_from_oracle(P0, "Glorious Anthem", false, ANTHEM)
        .as_enchantment()
        .with_mana_cost("{1}{W}{W}".parse().unwrap())
        .id();
    let thoughtlace = scenario
        .add_spell_to_hand_from_oracle(P0, "Thoughtlace", true, THOUGHTLACE)
        .with_mana_cost("{U}".parse().unwrap())
        .id();
    let disperse = scenario
        .add_spell_to_hand_from_oracle(P0, "Disperse", true, DISPERSE)
        .with_mana_cost("{1}{U}".parse().unwrap())
        .id();
    let candidates = [
        library_card(&mut scenario, P0, "White card", &[ManaColor::White]),
        library_card(&mut scenario, P0, "Blue card", &[ManaColor::Blue]),
    ];
    let spell = seek_spell(&mut scenario, P0, SEEK_EXCERPT);
    let mut runner = scenario.build();
    runner
        .cast(anthem)
        .resolve()
        .assert_zone(&[anthem], Zone::Battlefield);
    let original_incarnation = runner.state().objects[&anthem].incarnation;
    let colored = runner.cast(thoughtlace).target_object(anthem).resolve();
    colored.assert_zone(&[anthem], Zone::Battlefield);
    assert_eq!(
        colored.state().objects[&anthem].color,
        vec![ManaColor::Blue]
    );
    let bounced = runner.cast(disperse).target_object(anthem).resolve();
    bounced.assert_zone(&[anthem], Zone::Hand);
    assert!(bounced.events().iter().any(|e| matches!(e, GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Hand, .. } if *object_id == anthem)));
    assert_eq!(
        bounced.state().lki_cache[&anthem].colors,
        vec![ManaColor::Blue]
    );
    let recast = runner.cast(anthem).resolve();
    recast.assert_zone(&[anthem], Zone::Battlefield);
    let current = &recast.state().objects[&anthem];
    assert!(current.incarnation > original_incarnation);
    assert!(current.power.is_none() && current.toughness.is_none());
    // CR 400.7 + CR 608.2h: current white plus separate blue is two colors.
    // Cached blue plus separate blue would be one; both readings are guarded.
    assert_eq!(current.color, vec![ManaColor::White]);
    assert_eq!(
        recast.state().lki_cache[&anthem].colors,
        vec![ManaColor::Blue]
    );
    let outcome = runner.cast(spell).resolve();
    outcome.assert_hand_drawn(P0, 2);
    outcome.assert_zone(&candidates, Zone::Hand);
    assert_done(&outcome, spell);
}

#[test]
fn jev_seek_reads_board_after_public_response_without_announced_x() {
    assert_bound_seek(SEEK_EXCERPT, false);
    let mut scenario = scenario();
    scenario
        .add_enchantment_from_oracle(P0, "White stays", "")
        .with_color(vec![ManaColor::White]);
    let blue_reference = scenario
        .add_enchantment_from_oracle(P0, "Blue leaves", "")
        .with_color(vec![ManaColor::Blue])
        .id();
    let white = library_card(&mut scenario, P0, "White", &[ManaColor::White]);
    let blue = library_card(&mut scenario, P0, "Blue", &[ManaColor::Blue]);
    let spell = seek_spell(&mut scenario, P0, SEEK_EXCERPT);
    let response = scenario
        .add_spell_to_hand_from_oracle(P0, "Disperse", true, DISPERSE)
        .with_mana_cost("{1}{U}".parse().unwrap())
        .id();
    let mut runner = scenario.build();
    let mut committed = runner.cast(spell).commit();
    assert_eq!(committed.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(
        committed.state().objects[&blue_reference].zone,
        Zone::Battlefield
    );
    let outcome = committed
        .cast(response)
        .target_object(blue_reference)
        .resolve();
    // CR 107.3c + CR 608.2h: both the count and membership use the application board.
    outcome.assert_zone(&[blue_reference, white], Zone::Hand);
    outcome.assert_zone(&[blue], Zone::Library);
    outcome.assert_hand_drawn(P0, 2); // one bounced permanent and exactly one seek pick
    assert_done(&outcome, spell);
}

#[test]
fn jev_seek_count_frozen_before_delivery() {
    // Explicit synthetic destination variant of the public instruction.
    let text = "Seek X cards that each share a color with one or more permanents you control and put them onto the battlefield tapped, where X is the number of colors among permanents you control.";
    let mut scenario = scenario();
    scenario
        .add_enchantment_from_oracle(P0, "White reference", "")
        .with_color(vec![ManaColor::White]);
    let cards = [
        scenario
            .add_spell_to_library_top(P0, "White blue entrant", false)
            .as_enchantment()
            .with_color(vec![ManaColor::White, ManaColor::Blue])
            .id(),
        scenario
            .add_spell_to_library_top(P0, "White green entrant", false)
            .as_enchantment()
            .with_color(vec![ManaColor::White, ManaColor::Green])
            .id(),
    ];
    let parsed = parse_oracle_text(
        text,
        "Synthetic battlefield seek",
        &[],
        &["Instant".into()],
        &[],
    );
    assert!(matches!(
        parsed.abilities[0].effect.as_ref(),
        Effect::Seek {
            destination: Zone::Battlefield,
            count: QuantityExpr::Ref {
                qty: QuantityRef::DistinctColorsAmong { .. }
            },
            ..
        }
    ));
    assert!(parsed.parse_warnings.is_empty());
    let spell = seek_spell(&mut scenario, P0, text);
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    outcome.assert_hand_drawn(P0, 0);
    let entered: Vec<_> = cards
        .into_iter()
        .filter(|id| outcome.zone_of(*id) == Zone::Battlefield)
        .collect();
    // CR 608.2h: the first entry cannot increase this instruction's count.
    assert_eq!(entered.len(), 1);
    let entrant = &outcome.state().objects[&entered[0]];
    assert!(entrant.tapped);
    assert_eq!(entrant.entered_via_ability_source, Some(spell));
    assert_eq!(outcome.events().iter().filter(|e| matches!(e, GameEvent::ZoneChanged { object_id, from: Some(Zone::Library), to: Zone::Battlefield, .. } if cards.contains(object_id))).count(), 1);
    assert_done(&outcome, spell);
}

fn replacement_run(two: bool, reload: bool) -> (Vec<ObjectId>, Vec<GameEvent>) {
    // Explicit synthetic destination and independent tail, using real parsed replacements.
    let text = "Seek two cards and put them into your graveyard. You gain 3 life.";
    let parsed = parse_oracle_text(
        text,
        "Synthetic graveyard seek",
        &[],
        &["Instant".into()],
        &[],
    );
    assert!(matches!(
        parsed.abilities[0].effect.as_ref(),
        Effect::Seek {
            destination: Zone::Graveyard,
            ..
        }
    ));
    assert!(matches!(
        parsed.abilities[0]
            .sub_ability
            .as_deref()
            .unwrap()
            .effect
            .as_ref(),
        Effect::GainLife { .. }
    ));
    let mut scenario = scenario();
    let rip = scenario
        .add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE)
        .with_mana_cost("{1}{W}".parse().unwrap())
        .id();
    let leyline = two.then(|| {
        scenario
            .add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE)
            .with_mana_cost("{2}{B}{B}".parse().unwrap())
            .id()
    });
    let cards = [
        library_card(&mut scenario, P0, "First pick", &[ManaColor::White]),
        library_card(&mut scenario, P0, "Second pick", &[ManaColor::Blue]),
    ];
    let spell = seek_spell(&mut scenario, P0, text);
    let mut runner = scenario.build();
    assert!(!runner.state().objects[&rip]
        .replacement_definitions
        .is_empty());
    if let Some(leyline) = leyline {
        assert!(!runner.state().objects[&leyline]
            .replacement_definitions
            .is_empty());
    }
    let life = runner.state().players[0].life;
    let outcome = runner.cast(spell).resolve();
    let mut events = outcome.events().to_vec();
    if two {
        // CR 616.1: the real per-card replacement-order prompt is reached.
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert_eq!(
            runner.state().players[0].life,
            life,
            "tail waits for batch delivery"
        );
        let delivery = runner
            .state()
            .active_batch_delivery()
            .expect("picked tail is parked");
        assert_eq!(delivery.source_id, Some(spell));
        let saved = serde_json::to_vec(runner.state()).unwrap();
        if reload {
            runner = GameRunner::from_state(serde_json::from_slice::<GameState>(&saved).unwrap());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ));
            assert_eq!(
                runner.state().active_batch_delivery().unwrap().source_id,
                Some(spell)
            );
        }
    } else {
        assert_done(&outcome, spell);
        outcome.assert_life_delta(P0, 3);
    }
    let mut prompts = 0;
    for _ in 0..40 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::ReplacementChoice { .. } => {
                prompts += 1;
                // The same naturally produced actions feed AI and reducer clients.
                legal_actions(runner.state())
                    .into_iter()
                    .find(|action| matches!(action, GameAction::ChooseReplacement { index: 0 }))
                    .expect("engine offers a legal replacement action")
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            ref other => panic!("unexpected continuation state: {other:?}"),
        };
        events.extend(runner.act(action).unwrap().events);
    }
    if two {
        assert!(prompts >= 2, "both picked cards reach replacement ordering");
    }
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().players[0].life, life + 3);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::LifeChanged {
                    player_id: P0,
                    amount: 3,
                    ..
                }
            ))
            .count(),
        1,
        "independent tail executes once"
    );
    for card in cards {
        // CR 614.6: the replaced graveyard event never occurs.
        assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: Some(Zone::Library), to: Zone::Exile, .. } if *object_id == card)).count(), 1);
        assert!(!events.iter().any(|event| matches!(event, GameEvent::ZoneChanged { object_id, to: Zone::Graveyard, .. } if *object_id == card)));
    }
    assert!(!events
        .iter()
        .any(|event| matches!(event, GameEvent::CardDrawn { .. })));
    (cards.to_vec(), events)
}

#[test]
fn jev_seek_replacement_resume_and_reload() {
    let (control, _) = replacement_run(false, false);
    assert_eq!(
        control.len(),
        2,
        "one-replacement control derives two exiled picks"
    );
    let (ordinary, ordinary_events) = replacement_run(true, false);
    let (reloaded, reloaded_events) = replacement_run(true, true);
    assert_eq!(
        ordinary, reloaded,
        "paused reload cannot reroll picked identities"
    );
    let picked_moves = |events: &[GameEvent]| {
        events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Library),
                    to: Zone::Exile,
                    ..
                } => Some(*object_id),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        picked_moves(&ordinary_events),
        picked_moves(&reloaded_events),
        "same chosen order and undelivered tail"
    );
}

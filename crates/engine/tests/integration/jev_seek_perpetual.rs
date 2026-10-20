//! Public U5 selection and U6 exact settled-result perpetual consumers.

use engine::ai_support::legal_actions;
use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::parser::{oracle_nom::primitives::parse_mana_cost, parse_oracle_text};
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
use nom::{combinator::all_consuming, Parser};

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
    assert!(gaps.is_empty(), "complete instruction: {parsed:#?}");
    assert!(parsed.parse_warnings.is_empty(), "{parsed:#?}");
    if full {
        let seek = &parsed.abilities[0];
        let grant = seek.sub_ability.as_deref().expect("grant");
        assert!(grant.reads_return_result.is_some());
        assert_eq!(
            grant.reads_return_result.as_ref().map(|(id, _)| *id),
            seek.declares_return_result
        );
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
        .with_mana_cost(all_consuming(parse_mana_cost).parse("{1}{W}{W}").unwrap().1)
        .id();
    let thoughtlace = scenario
        .add_spell_to_hand_from_oracle(P0, "Thoughtlace", true, THOUGHTLACE)
        .with_mana_cost(all_consuming(parse_mana_cost).parse("{U}").unwrap().1)
        .id();
    let disperse = scenario
        .add_spell_to_hand_from_oracle(P0, "Disperse", true, DISPERSE)
        .with_mana_cost(all_consuming(parse_mana_cost).parse("{1}{U}").unwrap().1)
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
        .with_mana_cost(all_consuming(parse_mana_cost).parse("{1}{U}").unwrap().1)
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
        .with_mana_cost(all_consuming(parse_mana_cost).parse("{1}{W}").unwrap().1)
        .id();
    let leyline = two.then(|| {
        scenario
            .add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE)
            .with_mana_cost(all_consuming(parse_mana_cost).parse("{2}{B}{B}").unwrap().1)
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

const COST_GRANT: &str = "Those cards perpetually gain \"This spell costs {1} less to cast.\".";
const DIVINATION: &str = "Draw two cards.";
const CALL_TO_MIND: &str =
    "Return target instant or sorcery card from your graveyard to your hand.";
fn grants(state: &GameState, id: ObjectId) -> usize {
    state.objects[&id].perpetual_mods.iter().filter(|modification| matches!(modification,
        engine::types::ability::PerpetualModification::ModifyCost { mode: engine::types::statics::CostModifyMode::Reduce, amount }
        if *amount == ManaCost::generic(1))).count()
}
fn pool(runner: &mut GameRunner, types: &[ManaType]) {
    runner.state_mut().players[0].mana_pool = types
        .iter()
        .map(|kind| ManaUnit::new(*kind, ObjectId(0), false, vec![]))
        .collect();
}
fn complete_shape(text: &str) {
    let parsed = parse_oracle_text(text, "Labelled U6 synthetic", &[], &["Instant".into()], &[]);
    assert_eq!(parsed.abilities.len(), 1, "{parsed:#?}");
    assert!(parsed.parse_warnings.is_empty(), "{parsed:#?}");
    let nodes: Vec<_> = std::iter::successors(Some(&parsed.abilities[0]), |node| {
        node.sub_ability.as_deref()
    })
    .collect();
    assert!(nodes
        .iter()
        .any(|node| matches!(*node.effect, Effect::Seek { .. })));
    assert!(nodes.iter().any(|node| node.reads_return_result.is_some()));
    assert!(
        nodes
            .iter()
            .all(|node| !matches!(*node.effect, Effect::Unimplemented { .. })),
        "{parsed:#?}"
    );
}

fn circadian_runner() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId, ObjectId) {
    assert_bound_seek(FULL_CIRCADIAN, true);
    let mut scenario = scenario();
    scenario
        .add_enchantment_from_oracle(P0, "White blue reference", "")
        .with_color(vec![ManaColor::White, ManaColor::Blue]);
    scenario
        .add_enchantment_from_oracle(P0, "Repeated white", "")
        .with_color(vec![ManaColor::White]);
    scenario.add_artifact_from_oracle(P0, "Colorless reference", "");
    for index in 0..6 {
        library_card(&mut scenario, P0, &format!("Colorless filler {index}"), &[]);
    }
    let divination = scenario
        .add_spell_to_library_top(P0, "Divination", false)
        .with_mana_cost(all_consuming(parse_mana_cost).parse("{2}{U}").unwrap().1)
        .with_color(vec![ManaColor::Blue])
        .from_oracle_text(DIVINATION)
        .id();
    let disperse = scenario
        .add_spell_to_library_top(P0, "Disperse", true)
        .with_mana_cost(all_consuming(parse_mana_cost).parse("{1}{U}").unwrap().1)
        .with_color(vec![ManaColor::Blue])
        .from_oracle_text(DISPERSE)
        .id();
    let untouched = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION)
        .with_mana_cost(all_consuming(parse_mana_cost).parse("{2}{U}").unwrap().1)
        .with_color(vec![ManaColor::Blue])
        .id();
    let opponent = scenario
        .add_spell_to_hand_from_oracle(P1, "Divination", false, DIVINATION)
        .with_mana_cost(all_consuming(parse_mana_cost).parse("{2}{U}").unwrap().1)
        .with_color(vec![ManaColor::Blue])
        .id();
    let call = scenario
        .add_spell_to_hand_from_oracle(P0, "Call to Mind", false, CALL_TO_MIND)
        .with_mana_cost(all_consuming(parse_mana_cost).parse("{2}{U}").unwrap().1)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Circadian Struggle", true, FULL_CIRCADIAN)
        .with_mana_cost(
            all_consuming(parse_mana_cost)
                .parse("{4}{G/U}{G/U}")
                .unwrap()
                .1,
        )
        .id();
    let off_color = library_card(
        &mut scenario,
        P0,
        "Off-color same-name decoy",
        &[ManaColor::Black],
    );
    let mut runner = scenario.build();
    pool(
        &mut runner,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Blue,
            ManaType::Blue,
        ],
    );
    let outcome = runner.cast(spell).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    outcome.assert_hand_drawn(P0, 2);
    outcome.assert_zone(&[divination, disperse], Zone::Hand);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert!(
        runner.state().players[0].mana_pool.is_empty(),
        "actual hybrid payment"
    );
    assert_eq!(grants(runner.state(), divination), 2);
    assert_eq!(grants(runner.state(), disperse), 2);
    assert_eq!(runner.state().objects[&off_color].zone, Zone::Library);
    assert_eq!(grants(runner.state(), off_color), 0);
    for id in [untouched, opponent, spell, call] {
        assert_eq!(grants(runner.state(), id), 0);
    }
    (runner, divination, untouched, call, spell, disperse)
}

#[test]
fn jev_circadian_seek_grant_future_cast() {
    // CR 118.7a + CR 601.2f: generic reductions preserve the colored U component.
    let (mut runner, divination, untouched, _, _, _) = circadian_runner();
    pool(&mut runner, &[ManaType::Blue]);
    let mut control = GameRunner::from_state(runner.state().clone());
    assert!(
        control.cast(untouched).try_resolve().is_err(),
        "same-name unmodified card still needs 2U"
    );
    let mut control = GameRunner::from_state(runner.state().clone());
    pool(
        &mut control,
        &[ManaType::Colorless, ManaType::Colorless, ManaType::Blue],
    );
    let paid_control = control.cast(untouched).resolve();
    assert!(matches!(
        control.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(control.state().stack.is_empty());
    assert!(control.state().resolution_stack.is_empty());
    paid_control.assert_hand_drawn(P0, 2);
    paid_control.assert_zone(&[untouched], Zone::Graveyard);
    let outcome = runner.cast(divination).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    outcome.assert_hand_drawn(P0, 2);
    outcome.assert_zone(&[divination], Zone::Graveyard);
    assert!(
        runner.state().players[0].mana_pool.is_empty(),
        "generic reduction preserves U payment"
    );
    assert_eq!(grants(runner.state(), divination), 2);
}

#[test]
fn jev_circadian_perpetual_survives_zone_changes_and_reload() {
    let (mut runner, divination, untouched, call, source, _) = circadian_runner();
    pool(&mut runner, &[ManaType::Blue]);
    let first = runner.cast(divination).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    first.assert_hand_drawn(P0, 2);
    first.assert_zone(&[divination, source], Zone::Graveyard);
    let old_incarnation = runner.state().objects[&divination].incarnation;
    runner = GameRunner::from_state(
        serde_json::from_slice::<GameState>(&serde_json::to_vec(runner.state()).unwrap()).unwrap(),
    );
    pool(
        &mut runner,
        &[ManaType::Colorless, ManaType::Colorless, ManaType::Blue],
    );
    let returned = runner.cast(call).target_object(divination).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    returned.assert_zone(&[divination], Zone::Hand);
    assert_eq!(grants(runner.state(), divination), 2);
    assert!(runner.state().objects[&divination].incarnation > old_incarnation);
    pool(&mut runner, &[ManaType::Blue]);
    let recast = runner.cast(divination).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    recast.assert_hand_drawn(P0, 2);
    recast.assert_zone(&[divination], Zone::Graveyard);
    assert_eq!(grants(runner.state(), untouched), 0);
    assert_eq!(grants(runner.state(), divination), 2);
}

#[test]
fn jev_circadian_empty_result_replaces_prior_producer() {
    for second in [
        "Seek zero cards.",
        "Seek a creature card.",
        "Seek two cards.",
    ] {
        let text = format!("Seek a blue card. {COST_GRANT} {second} {COST_GRANT} You gain 4 life.");
        complete_shape(&text);
        let mut scenario = scenario();
        let decoy = (second != "Seek two cards.").then(|| {
            library_card(
                &mut scenario,
                P0,
                "Nonmatching resident",
                &[ManaColor::Black],
            )
        });
        let first = library_card(&mut scenario, P0, "Prior result", &[ManaColor::Blue]);
        let source = seek_spell(&mut scenario, P0, &text);
        let mut runner = scenario.build();
        let outcome = runner.cast(source).resolve();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        outcome.assert_zone(&[first], Zone::Hand);
        outcome.assert_life_delta(P0, 4);
        assert_eq!(
            grants(runner.state(), first),
            1,
            "first named result positively granted once; fresh empty cannot grant it again"
        );
        assert_eq!(grants(runner.state(), source), 0);
        if let Some(decoy) = decoy {
            assert_eq!(runner.state().objects[&decoy].zone, Zone::Library);
            assert_eq!(grants(runner.state(), decoy), 0);
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
    let text = format!("Seek five cards. {COST_GRANT} You gain 4 life.");
    complete_shape(&text);
    let mut scenario = scenario();
    let card = library_card(
        &mut scenario,
        P0,
        "Insufficient but present",
        &[ManaColor::Blue],
    );
    let source = seek_spell(&mut scenario, P0, &text);
    let mut runner = scenario.build();
    let outcome = runner.cast(source).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    outcome.assert_zone(&[card], Zone::Hand);
    outcome.assert_life_delta(P0, 4);
    assert_eq!(grants(runner.state(), card), 1);
}

fn sought_replacement_run(two: bool, reload_at: Option<usize>) -> (Vec<ObjectId>, Vec<GameEvent>) {
    // Labelled synthetic destination; the GainLife is a complete independent
    // instruction, so the named reader is beyond the continuation head.
    let text =
        format!("Seek two cards and put them into your graveyard. You gain 7 life. {COST_GRANT}");
    complete_shape(&text);
    let mut scenario = scenario();
    scenario
        .add_enchantment_from_oracle(P0, "Rest in Peace", REST_IN_PEACE)
        .with_mana_cost(all_consuming(parse_mana_cost).parse("{1}{W}").unwrap().1);
    if two {
        scenario
            .add_enchantment_from_oracle(P1, "Leyline of the Void", LEYLINE)
            .with_mana_cost(all_consuming(parse_mana_cost).parse("{2}{B}{B}").unwrap().1);
    }
    let cards = [
        library_card(&mut scenario, P0, "First chosen", &[ManaColor::Blue]),
        library_card(&mut scenario, P0, "Second chosen", &[ManaColor::White]),
    ];
    let source = seek_spell(&mut scenario, P0, &text);
    let mut runner = scenario.build();
    let life = runner.state().players[0].life;
    let outcome = runner.cast(source).resolve();
    let mut events = outcome.events().to_vec();
    let mut prompts = 0;
    for _ in 0..50 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::ReplacementChoice { .. } => {
                assert_eq!(
                    runner.state().players[0].life,
                    life,
                    "nonhead consumer waits for whole batch"
                );
                for card in cards {
                    assert_eq!(grants(runner.state(), card), 0);
                }
                let completion = runner
                    .state()
                    .active_batch_delivery()
                    .expect("real parked batch")
                    .completion
                    .as_ref()
                    .expect("named result completion");
                let engine::types::game_state::BatchCompletion::RecordInstructionZoneResult {
                    occurrence_id,
                    return_result_process,
                    result_id,
                    settled_records,
                } = completion
                else {
                    panic!("{completion:?}")
                };
                assert!(settled_records.is_none());
                assert_eq!(
                    runner.state().return_result_frames[occurrence_id].process,
                    *return_result_process
                );
                assert!(!runner.state().return_result_frames[occurrence_id]
                    .results
                    .contains_key(result_id));
                let key = (*occurrence_id, *result_id);
                if reload_at == Some(prompts) {
                    let trusted = engine::types::game_state::PersistedGameState::capture(
                        runner.state().clone(),
                    );
                    let loaded: engine::types::game_state::PersistedGameState =
                        serde_json::from_slice(&serde_json::to_vec(&trusted).unwrap()).unwrap();
                    runner = GameRunner::from_state(loaded.into_game_state().unwrap());
                    assert!(runner.state().return_result_frames.contains_key(&key.0));
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::ReplacementChoice { .. }
                    ));
                }
                prompts += 1;
                legal_actions(runner.state())
                    .into_iter()
                    .find(|a| matches!(a, GameAction::ChooseReplacement { index: 0 }))
                    .expect("legal response")
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            ref other => panic!("unexpected {other:?}"),
        };
        events.extend(runner.act(action).unwrap().events);
    }
    assert_eq!(prompts, if two { 2 } else { 0 });
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().players[0].life, life + 7);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::LifeChanged {
                    player_id: P0,
                    amount: 7,
                    ..
                }
            ))
            .count(),
        1
    );
    for card in cards {
        assert_eq!(runner.state().objects[&card].zone, Zone::Exile);
        assert_eq!(grants(runner.state(), card), 1);
    }
    assert_eq!(grants(runner.state(), source), 0);
    (cards.to_vec(), events)
}
#[test]
fn jev_sought_grant_waits_for_complete_replacement_batch() {
    sought_replacement_run(false, None);
    sought_replacement_run(true, None);
}
#[test]
fn jev_sought_result_current_pause_reload() {
    let (cards, events) = sought_replacement_run(true, None);
    for pause in [0, 1] {
        let (loaded_cards, loaded_events) = sought_replacement_run(true, Some(pause));
        assert_eq!(cards, loaded_cards);
        let moves = |events: &[GameEvent]| {
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
        assert_eq!(moves(&events), moves(&loaded_events));
    }
}

#[test]
fn jev_sought_entry_pause_finishes_before_grant() {
    let text =
        format!("Seek an Aura card and put it onto the battlefield. You gain 7 life. {COST_GRANT}");
    complete_shape(&text);
    for hosts in [0, 2] {
        let mut scenario = scenario();
        for index in 0..hosts {
            scenario.add_creature_from_oracle(P0, &format!("Legal host {index}"), 2, 2, "");
        }
        let aura = scenario
            .add_spell_to_library_top(P0, "Labelled synthetic Aura", false)
            .as_enchantment()
            .with_subtypes(vec!["Aura"])
            .from_oracle_text("Enchant creature\nEnchanted creature can't attack or block.")
            .id();
        let source = seek_spell(&mut scenario, P0, &text);
        let mut runner = scenario.build();
        let life = runner.state().players[0].life;
        let outcome = runner.cast(source).resolve();
        if hosts == 0 {
            // CR 303.4g: an unhosted library Aura remains in its current zone.
            outcome.assert_zone(&[aura], Zone::Library);
            outcome.assert_life_delta(P0, 7);
            assert_eq!(grants(runner.state(), aura), 0);
        } else {
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReturnAsAuraTarget { .. }
            ));
            assert_eq!(runner.state().players[0].life, life);
            assert_eq!(grants(runner.state(), aura), 0);
            assert!(runner.state().active_batch_delivery().is_some());
            let action = legal_actions(runner.state())
                .into_iter()
                .next()
                .expect("legal Aura host response");
            runner.act(action).unwrap();
            for _ in 0..20 {
                if runner.state().stack.is_empty()
                    && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            assert_eq!(runner.state().objects[&aura].zone, Zone::Battlefield);
            assert_eq!(
                runner.state().objects[&aura].entered_via_ability_source,
                Some(source)
            );
            assert_eq!(runner.state().players[0].life, life + 7);
            assert_eq!(grants(runner.state(), aura), 1);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
    }
}

#[test]
fn jev_sought_grant_uses_grant_start_color_count() {
    let text = "Seek X cards and put them onto the battlefield, where X is the number of colors among permanents you control. For each color among permanents you control, those cards perpetually gain \"This spell costs {1} less to cast.\"";
    complete_shape(text);
    let mut scenario = scenario();
    scenario
        .add_enchantment_from_oracle(P0, "Initially white", "")
        .with_color(vec![ManaColor::White]);
    let card = scenario
        .add_spell_to_library_top(P0, "Blue green entrant", false)
        .as_enchantment()
        .with_color(vec![ManaColor::Blue, ManaColor::Green])
        .id();
    let source = seek_spell(&mut scenario, P0, text);
    let mut runner = scenario.build();
    let outcome = runner.cast(source).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    outcome.assert_zone(&[card], Zone::Battlefield);
    assert_eq!(outcome.events().iter().filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: Some(Zone::Library), to: Zone::Battlefield, .. } if *object_id == card)).count(), 1);
    assert_eq!(
        grants(runner.state(), card),
        3,
        "Seek1 followed by grant-start WUG3"
    );
}

#[test]
fn jev_perpetual_recipient_authorities_remain_distinct() {
    let text = "Seek three nonland cards, then nonland cards in your hand perpetually gain \"This spell costs {1} less to cast.\"";
    let mut scenario = scenario();
    let existing = scenario
        .add_spell_to_hand_from_oracle(P0, "Existing nonland", true, "Draw a card.")
        .id();
    let land = scenario.add_land_to_hand(P0, "Island").id();
    let cards: Vec<_> = (0..3)
        .map(|index| {
            library_card(
                &mut scenario,
                P0,
                &format!("Sought nonland {index}"),
                &[ManaColor::Blue],
            )
        })
        .collect();
    let source = scenario
        .add_spell_to_hand_from_oracle(P0, "Discover the Formula", true, text)
        .with_mana_cost(all_consuming(parse_mana_cost).parse("{4}{U}{U}").unwrap().1)
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(source).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    outcome.assert_hand_drawn(P0, 3);
    outcome.assert_zone(&cards, Zone::Hand);
    for card in cards.into_iter().chain([existing]) {
        assert_eq!(grants(runner.state(), card), 1);
    }
    assert_eq!(grants(runner.state(), land), 0);
    assert_eq!(grants(runner.state(), source), 0);
}

#[test]
fn jev_sought_same_clause_distinct_occurrences() {
    let text = "{1}: Seek a card from among the top one cards of your library. Those cards perpetually gain \"This spell costs {1} less to cast.\"";
    let mut scenario = scenario();
    let first_source = scenario
        .add_artifact_from_oracle(P0, "First labelled Seek source", text)
        .id();
    let second_source = scenario
        .add_artifact_from_oracle(P0, "Second labelled Seek source", text)
        .id();
    let cards: Vec<_> = (0..3)
        .map(|index| {
            library_card(
                &mut scenario,
                P0,
                &format!("Forced top member {index}"),
                &[ManaColor::Blue],
            )
        })
        .collect();
    let mut runner = scenario.build();
    let first_id = runner.state().objects[&first_source].abilities[0].declares_return_result;
    assert!(first_id.is_some());
    assert_eq!(
        first_id,
        runner.state().objects[&second_source].abilities[0].declares_return_result
    );
    let mut picked = Vec::new();
    let mut allocator = runner.state().next_return_result_occurrence_id;
    for source in [first_source, first_source, second_source] {
        let expected = *runner.state().library_of(P0).front().unwrap();
        let outcome = runner.activate(source, 0).resolve();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
        assert_eq!(outcome.state().objects[&expected].zone, Zone::Hand);
        assert!(
            runner.state().next_return_result_occurrence_id > allocator,
            "every resolution allocates its own occurrence"
        );
        allocator = runner.state().next_return_result_occurrence_id;
        picked.push(expected);
        for card in &cards {
            assert_eq!(
                grants(runner.state(), *card),
                usize::from(picked.contains(card))
            );
        }
        for source in [first_source, second_source] {
            assert_eq!(grants(runner.state(), source), 0);
        }
        assert!(runner.state().return_result_frames.is_empty());
    }
    picked.sort_unstable();
    picked.dedup();
    assert_eq!(picked.len(), 3);
}

#[test]
fn jev_sought_activated_grant_survives_source_departure() {
    // CR 113.7a: the activated ability survives its sacrificed source.
    let text = "{1}, Sacrifice this artifact: Seek a card. Those cards perpetually gain \"This spell costs {1} less to cast.\"";
    let mut scenario = scenario();
    let source = scenario
        .add_artifact_from_oracle(P0, "Labelled departing Seek source", text)
        .id();
    let card = library_card(
        &mut scenario,
        P0,
        "Exact recipient after source departure",
        &[ManaColor::Blue],
    );
    let mut runner = scenario.build();
    let outcome = runner.activate(source, 0).pay_with(&[source]).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert_eq!(outcome.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(outcome.state().objects[&card].zone, Zone::Hand);
    assert_eq!(grants(runner.state(), card), 1);
    assert_eq!(grants(runner.state(), source), 0);
}

#[test]
fn jev_perpetual_legacy_they_conjure_chosen_and_self_public() {
    // Labelled cost-grant sisters of the retained full Indris/Racketeer suite.
    let mut scenario = scenario();
    let text = "Conjure two cards named Divination into your hand. They perpetually gain \"This spell costs {1} less to cast.\"";
    let conjure = seek_spell(&mut scenario, P0, text);
    let chosen_text = "Choose a creature card in your hand. They perpetually gain \"This spell costs {1} less to cast.\"";
    let chosen = seek_spell(&mut scenario, P0, chosen_text);
    let self_text = "This card perpetually gains \"This spell costs {1} less to cast.\"";
    let self_grant = seek_spell(&mut scenario, P0, self_text);
    let creature = scenario
        .add_creature_to_hand(P0, "Selected creature", 2, 2)
        .id();
    let decoy = scenario.add_spell_to_hand(P0, "Unselected card", true).id();
    let mut runner = scenario.build();
    let outcome = runner.cast(conjure).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    outcome.assert_hand_drawn(P0, 2);
    let conjured: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.name == "Divination")
        .map(|object| object.id)
        .collect();
    assert_eq!(conjured.len(), 2);
    for card in conjured {
        assert_eq!(grants(runner.state(), card), 1);
    }
    runner.cast(chosen).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseFromZoneChoice { .. }
    ));
    let action = legal_actions(runner.state()).into_iter().find(|action| matches!(action, GameAction::SelectCards { cards } if cards == &vec![creature])).expect("actual chosen-card response");
    runner.act(action).unwrap();
    for _ in 0..20 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(grants(runner.state(), creature), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    runner.cast(self_grant).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert_eq!(grants(runner.state(), self_grant), 1);
    for id in [conjure, chosen, decoy] {
        assert_eq!(grants(runner.state(), id), 0);
    }
}

#[test]
fn jev_perpetual_event_recipient_remains_distinct() {
    let mut scenario = scenario();
    let whelp = scenario.add_creature_from_oracle(P0, "Darigaaz's Whelp", 2, 2,
        "Kicker {1}{R}\nFlying\nWhenever you draw a Dragon card, it perpetually gets +1/+1.\nWhen Darigaaz's Whelp enters, if it was kicked, seek a Dragon card. Darigaaz's Whelp and that Dragon card each perpetually get +1/+1.")
        .with_mana_cost(all_consuming(parse_mana_cost).parse("{1}{B}").unwrap().1).id();
    let dragon = scenario
        .add_spell_to_library_top(P0, "Drawn Dragon", false)
        .as_creature()
        .with_subtypes(vec!["Dragon"])
        .id();
    let other = library_card(&mut scenario, P0, "Unrelated draw", &[]);
    let draw = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION)
        .with_mana_cost(all_consuming(parse_mana_cost).parse("{2}{U}").unwrap().1)
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(draw).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    outcome.assert_hand_drawn(P0, 2);
    outcome.assert_zone(&[dragon, other], Zone::Hand);
    assert_eq!(runner.state().objects[&dragon].perpetual_mods.len(), 1);
    assert!(runner.state().objects[&whelp].perpetual_mods.is_empty());
    assert!(runner.state().objects[&other].perpetual_mods.is_empty());
}

#[test]
fn jev_sought_grant_preserves_independently_targeted_tail() {
    let text = format!("Seek a card. {COST_GRANT} Target creature gets +1/+1 until end of turn.");
    complete_shape(&text);
    let mut scenario = scenario();
    let card = library_card(&mut scenario, P0, "Exact sought card", &[ManaColor::Blue]);
    let target = scenario
        .add_creature_from_oracle(P0, "Independent tail target", 2, 2, "")
        .id();
    let source = seek_spell(&mut scenario, P0, &text);
    let mut runner = scenario.build();
    let outcome = runner.cast(source).target_object(target).resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    outcome.assert_zone(&[card], Zone::Hand);
    outcome.assert_power_toughness(target, 3, 3);
    assert_eq!(grants(runner.state(), card), 1);
    assert_eq!(grants(runner.state(), target), 0);
    assert_eq!(grants(runner.state(), source), 0);
}

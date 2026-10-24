//! Complete printed hand-choice bodies through paid casting and public replies.
use engine::game::derived_views::ClientGameStateRef;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::types::ability::{
    ChoiceType, ChosenAttribute, Effect, EffectKind, FilterProp, TargetFilter, TargetRef,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, GameState, PersistedGameState, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolution::ResolutionStateWire;
use engine::types::resolved_commands::{
    ResolvedInformationAudience, ResolvedInformationEdit, ResolvedInformationLifetime,
};
use engine::types::zones::Zone;
use serde_json::{json, Value};

const DURESS: &str = "Target opponent reveals their hand. You choose a noncreature, nonland card from it. That player discards that card.";
const ADDLE: &str = "Choose a color. Target player reveals their hand and you choose a card of that color from it. That player discards that card.";
const LAY_BARE: &str = "Target opponent reveals their hand. You choose a nonlegendary, nonland card from it. That player discards that card.";
const THOUGHTSEIZE: &str = "Target player reveals their hand. You choose a nonland card from it. That player discards that card. You lose 2 life.";
const DREAD: &str = "Cleave {2}{B} (You may cast this spell for its cleave cost. If you do, remove the words in square brackets.)\nTarget player reveals their hand. You choose a nonland card from it [with mana value 2 or less]. That player discards that card.";
const MARDU: &str = "Choose one —\n• Mardu Charm deals 4 damage to target creature.\n• Create two 1/1 white Warrior creature tokens. They gain first strike until end of turn.\n• Target opponent reveals their hand. You choose a noncreature, nonland card from it. That player discards that card.";
const NIGHTMARE: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI — Destroy target creature an opponent controls with power 2 or less.\nII — Target opponent reveals their hand. You choose a noncreature, nonland card from it. That player discards that card.\nIII — Exile target opponent's graveyard.";

fn black_cost(generic: u32) -> ManaCost {
    ManaCost::Cost {
        generic,
        shards: vec![ManaCostShard::Black],
    }
}

fn scenario() -> GameScenario {
    let mut scene = GameScenario::new_n_player(3, 37);
    scene.at_phase(Phase::PreCombatMain);
    // Real pool resources are consumed by the normal payment authority.
    scene.with_mana_pool(
        P0,
        vec![
            ManaType::Black,
            ManaType::Black,
            ManaType::Black,
            ManaType::Red,
            ManaType::White,
        ]
        .into_iter()
        .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect(),
    );
    for player in [P0, P1, PlayerId(2)] {
        scene.with_library_top(
            player,
            &[
                "Private library one",
                "Private library two",
                "Private library three",
                "Private library four",
            ],
        );
    }
    scene
}

struct Hand {
    first: ObjectId,
    second: ObjectId,
    creature: ObjectId,
    legendary: ObjectId,
    land: ObjectId,
    foreign: ObjectId,
}

fn hand(scene: &mut GameScenario) -> Hand {
    let first = scene
        .add_spell_to_hand(P1, "Hand first MV2", false)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let second = scene
        .add_spell_to_hand(P1, "Hand second MV3", true)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let creature = scene
        .add_creature_to_hand(P1, "Nonlegendary creature", 2, 2)
        .id();
    let legendary = scene
        .add_creature_to_hand(P1, "Legendary creature", 3, 3)
        .as_legendary()
        .id();
    let land = scene.add_land_to_hand(P1, "Hand land").id();
    let foreign = scene
        .add_spell_to_hand(PlayerId(2), "Other player's secret", true)
        .id();
    Hand {
        first,
        second,
        creature,
        legendary,
        land,
        foreign,
    }
}

fn spell(scene: &mut GameScenario, name: &str, text: &str, cost: ManaCost) -> ObjectId {
    scene
        .add_spell_to_hand_from_oracle(P0, name, name == "Mardu Charm", text)
        .with_mana_cost(cost)
        .id()
}

fn prompt(runner: &GameRunner, expected: &[ObjectId], optional: bool) {
    let WaitingFor::RevealChoice {
        player,
        cards,
        optional: actual_optional,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "required public hand choice: {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!(*player, P0);
    assert_eq!(*actual_optional, optional);
    let mut actual = cards.clone();
    actual.sort();
    let mut expected = expected.to_vec();
    expected.sort();
    assert_eq!(actual, expected);
}

fn settle(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..96 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. }
                if runner.state().stack.is_empty()
                    && runner.state().trigger_stack.is_empty()
                    && runner.state().resolution_stack.is_empty() =>
            {
                return
            }
            WaitingFor::Priority { .. } => {
                events.extend(runner.act(GameAction::PassPriority).unwrap().events)
            }
            _ => return,
        }
    }
    panic!("bounded completion did not settle");
}

fn terminal(runner: &GameRunner) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().trigger_stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().owed_spell_final_part().is_none());
}

fn answer(runner: &mut GameRunner, selected: ObjectId) -> Vec<GameEvent> {
    let mut events = runner
        .act(GameAction::SelectCards {
            cards: vec![selected],
        })
        .unwrap()
        .events;
    settle(runner, &mut events);
    events
}

fn unchanged_hand(runner: &GameRunner, ids: &[ObjectId]) {
    for id in ids {
        assert_eq!(runner.state().objects[id].zone, Zone::Hand, "decoy {id:?}");
    }
}

// SHAPE objects have printed colors/types, no remembered choice or resolution state.
fn color_hand(scene: &mut GameScenario) -> (Vec<(ObjectId, Vec<ManaColor>)>, ObjectId) {
    let mut cards = vec![];
    for color in [
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ] {
        let first = scene
            .add_spell_to_hand(P1, &format!("Printed {color:?} first"), false)
            .with_color(vec![color])
            .id();
        let land_colors = if color == ManaColor::White {
            vec![color]
        } else {
            vec![color, ManaColor::White]
        };
        let second = scene
            .add_land_to_hand(P1, &format!("Printed {color:?} land"))
            .with_color(land_colors.clone())
            .id();
        cards.push((first, vec![color]));
        cards.push((second, land_colors));
    }
    let colorless = scene.add_land_to_hand(P1, "Colorless decoy").id();
    cards.push((colorless, vec![]));
    let foreign = scene
        .add_spell_to_hand(PlayerId(2), "Foreign blue secret", true)
        .with_color(vec![ManaColor::Blue])
        .id();
    (cards, foreign)
}

fn color_prompt(runner: &GameRunner) -> engine::types::game_state::TriggerSourceContext {
    let WaitingFor::NamedChoice {
        player,
        choice_type,
        options,
        source: Some(source),
        persist_player,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("actual color prompt: {:?}", runner.state().waiting_for);
    };
    assert_eq!(*player, P0);
    assert_eq!(*choice_type, ChoiceType::color());
    assert_eq!(options, &["White", "Blue", "Black", "Red", "Green"]);
    assert_eq!(*persist_player, None);
    assert_eq!(
        source.binding,
        engine::types::game_state::NamedChoiceSourceBinding::ResolutionContext
    );
    assert!(source.has_matching_context());
    source.context.clone().unwrap()
}

fn addle_envelopes(runner: &GameRunner, offered: Option<&[ObjectId]>) {
    let before = serde_json::to_value(runner.state()).unwrap();
    for viewer in [Some(P0), Some(P1), Some(PlayerId(2)), None] {
        for wire in envelopes(runner.state(), viewer) {
            assert!(wire["state"].is_object());
            assert!(serde_json::from_value::<PersistedGameState>(wire["state"].clone()).is_err());
            let prompt = &wire["state"]["waiting_for"];
            if let Some(ids) = offered {
                assert_eq!(prompt["type"], "RevealChoice");
                assert_eq!(prompt["data"]["cards"], json!(ids));
                assert_eq!(prompt["data"]["player"], json!(P0));
                assert_eq!(
                    wire["derived"]["unique_authorized_submitter"],
                    json!(runner.state().turn_decision_controller.unwrap_or(P0))
                );
                assert!(wire.to_string().contains("Printed Blue first"));
                assert!(wire.to_string().contains("Colorless decoy"));
            } else if matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }) {
                assert_eq!(prompt["type"], "NamedChoice");
                assert_eq!(prompt["data"]["player"], json!(P0));
                assert_eq!(
                    prompt["data"]["options"],
                    json!(["White", "Blue", "Black", "Red", "Green"])
                );
                assert!(prompt["data"]["source"]["prompt"].is_object());
                assert!(prompt["data"]["source"].get("context").is_none());
                assert_eq!(
                    wire.to_string().contains("Printed Blue first"),
                    viewer == Some(P1)
                );
            } else {
                assert_eq!(prompt["type"], "Priority");
            }
            assert_eq!(
                wire.to_string().contains("Foreign blue secret"),
                viewer == Some(PlayerId(2))
            );
            assert!(!wire.to_string().contains("Private library one"));
            assert!(wire["state"].get("private_look_ids").is_none());
            assert!(wire["state"].get("private_look_player").is_none());
            assert!(!wire["state"]["resolution_stack"]
                .to_string()
                .contains("trigger_source"));
        }
    }
    assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
}

fn addle_disposal_capture(
    runner: &GameRunner,
    source: ObjectId,
    expected: &engine::types::game_state::TriggerSourceContext,
) {
    let pending = runner
        .state()
        .active_ability_continuation()
        .expect("real top disposal owner");
    assert_eq!(
        pending.chain.effect,
        Effect::DiscardCard {
            target: TargetFilter::ParentTarget,
            count: 1
        }
    );
    assert_eq!(pending.chain.source_id, source);
    assert_eq!(expected.identity.reference.object_id, source);
    assert_eq!(
        pending.chain.source_incarnation,
        Some(expected.identity.reference.incarnation)
    );
    assert_eq!(pending.chain.controller, P0);
    assert!(pending.chain.cast_occurrence.is_some());
    assert_eq!(pending.chain.trigger_definition_ref, None);
    assert_eq!(pending.chain.scoped_player, None);
    assert_eq!(pending.chain.trigger_source.as_ref(), Some(expected));
    assert!(runner.state().objects[&source].chosen_attributes.is_empty());
}

#[test]
fn contextual_hand_addle_owned_color_revalidation_and_both_envelopes() {
    // CR 105.4 + CR 608.2c + CR 608.2d + CR 701.9a + CR 701.9b: the answer qualifies a Card, including a colored land.
    let mut scene = scenario();
    let (cards, foreign) = color_hand(&mut scene);
    let source = spell(&mut scene, "Addle", ADDLE, black_cost(1));
    let mut runner = scene.build();
    let libraries: Vec<_> = runner
        .state()
        .players
        .iter()
        .map(|player| player.library.clone())
        .collect();
    let paid = runner.cast(source).target_player(P1).resolve();
    let before_capture = color_prompt(&runner);
    assert_eq!(before_capture.identity.reference.object_id, source);
    assert_eq!(before_capture.identity.expected_zone, Zone::Stack);
    let original_tail = runner
        .state()
        .active_ability_continuation()
        .unwrap()
        .chain
        .clone();
    assert!(matches!(original_tail.effect, Effect::RevealHand { .. }));
    let mut expected = before_capture.clone();
    expected
        .lki
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Blue));
    let offered: Vec<_> = cards
        .iter()
        .filter(|(_, colors)| colors.contains(&ManaColor::Blue))
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(offered.len(), 2);
    assert_ne!(offered[0], offered[1]);
    let selected = offered[1];
    assert!(runner.state().objects[&selected]
        .chosen_attributes
        .is_empty());
    addle_envelopes(&runner, None);
    let first_pause = restored_forms(runner.state());
    for invalid in [
        GameAction::ChooseOption {
            choice: "Colorless".into(),
        },
        GameAction::ChooseOption {
            choice: "Multicolored".into(),
        },
    ] {
        let mut state = runner.state().clone();
        assert!(engine::apply(&mut state, P0, invalid).is_err());
    }
    let mut wrong_actor = runner.state().clone();
    assert!(engine::apply(
        &mut wrong_actor,
        P1,
        GameAction::ChooseOption {
            choice: "Blue".into()
        }
    )
    .is_err());
    runner
        .act(GameAction::ChooseOption {
            choice: "Blue".into(),
        })
        .unwrap();
    prompt(&runner, &offered, false);
    addle_disposal_capture(&runner, source, &expected);
    let chain = &runner.state().active_ability_continuation().unwrap().chain;
    assert_eq!(chain.original_controller, original_tail.original_controller);
    assert_eq!(chain.cast_occurrence, original_tail.cast_occurrence);
    assert_eq!(
        chain.trigger_definition_ref,
        original_tail.trigger_definition_ref
    );
    assert_eq!(chain.scoped_player, original_tail.scoped_player);
    let filter =
        TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::IsChosenColor]));
    assert!(
        matches!(&runner.state().waiting_for, WaitingFor::RevealChoice { filter: actual, .. } if actual == &filter)
    );
    assert!(engine::game::filter::matches_target_filter(
        runner.state(),
        selected,
        &filter,
        &engine::game::filter::FilterContext::from_ability(chain)
    ));
    addle_envelopes(&runner, Some(&offered));
    for state in first_pause {
        let mut restored = GameRunner::from_state(state);
        color_prompt(&restored);
        restored
            .act(GameAction::ChooseOption {
                choice: "Blue".into(),
            })
            .unwrap();
        prompt(&restored, &offered, false);
        addle_disposal_capture(&restored, source, &expected);
        answer(&mut restored, selected);
        assert_eq!(restored.state().objects[&selected].zone, Zone::Graveyard);
        assert_eq!(restored.state().objects[&selected].owner, P1);
        assert_eq!(
            restored
                .state()
                .players
                .iter()
                .map(|player| player.library.clone())
                .collect::<Vec<_>>(),
            libraries
        );
        unchanged_hand(
            &restored,
            &cards
                .iter()
                .filter(|(id, _)| *id != selected)
                .map(|(id, _)| *id)
                .chain([foreign])
                .collect::<Vec<_>>(),
        );
        terminal(&restored);
    }
    for state in restored_forms(runner.state()) {
        let mut restored = GameRunner::from_state(state);
        addle_disposal_capture(&restored, source, &expected);
        answer(&mut restored, selected);
        assert_eq!(restored.state().objects[&selected].zone, Zone::Graveyard);
        assert_eq!(restored.state().objects[&selected].owner, P1);
        assert_eq!(
            restored
                .state()
                .players
                .iter()
                .map(|player| player.library.clone())
                .collect::<Vec<_>>(),
            libraries
        );
        unchanged_hand(
            &restored,
            &cards
                .iter()
                .filter(|(id, _)| *id != selected)
                .map(|(id, _)| *id)
                .chain([foreign])
                .collect::<Vec<_>>(),
        );
        terminal(&restored);
    }
    for invalid in [
        vec![],
        vec![offered[0], selected],
        vec![foreign],
        vec![cards.last().unwrap().0],
    ] {
        let mut state = runner.state().clone();
        assert!(engine::apply(&mut state, P0, GameAction::SelectCards { cards: invalid }).is_err());
    }
    for corrupt in 0..5 {
        let mut state = runner.state().clone();
        // A matching global value and live candidate cannot replace this owner.
        state.named_color_this_resolution = Some(ManaColor::Blue);
        let chain = &mut state
            .active_ability_continuation_frame_mut()
            .unwrap()
            .pending
            .chain;
        match corrupt {
            0 => chain.trigger_source = None,
            1 => chain
                .trigger_source
                .as_mut()
                .unwrap()
                .lki
                .chosen_attributes
                .clear(),
            2 => {
                chain
                    .trigger_source
                    .as_mut()
                    .unwrap()
                    .identity
                    .reference
                    .object_id = foreign
            }
            3 => chain.source_incarnation = Some(u64::MAX),
            4 => chain.original_controller = Some(P1),
            _ => unreachable!(),
        }
        assert!(engine::apply(
            &mut state,
            P0,
            GameAction::SelectCards {
                cards: vec![selected]
            }
        )
        .is_err());
        assert_eq!(state.objects[&selected].zone, Zone::Hand);
    }
    let mut wrong_actor = runner.state().clone();
    assert!(engine::apply(
        &mut wrong_actor,
        P1,
        GameAction::SelectCards {
            cards: vec![selected]
        }
    )
    .is_err());
    let events = answer(&mut runner, selected);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::DiscardCard,
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(paid.events.iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::Choose,
            ..
        }
    )));
    assert_eq!(runner.state().objects[&selected].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&selected].owner, P1);
    assert_eq!(
        runner
            .state()
            .players
            .iter()
            .map(|player| player.library.clone())
            .collect::<Vec<_>>(),
        libraries
    );
    unchanged_hand(
        &runner,
        &cards
            .iter()
            .filter(|(id, _)| *id != selected)
            .map(|(id, _)| *id)
            .chain([foreign])
            .collect::<Vec<_>>(),
    );
    terminal(&runner);
    addle_envelopes(&runner, None);
    for viewer in [Some(P0), Some(P1), Some(PlayerId(2)), None] {
        for wire in envelopes(runner.state(), viewer) {
            assert!(
                wire.to_string().contains("Printed Blue land"),
                "the selected owner-graveyard face is public"
            );
            assert_eq!(wire["state"]["waiting_for"]["type"], "Priority");
        }
    }
}

#[test]
fn contextual_hand_random_color_uses_owned_resolution_context() {
    // SHAPE, paid 1B. Keep the period and actual SequentialSibling edge.
    const RANDOM: &str = "Choose a color at random. Target player reveals their hand and you choose a card of that color from it. That player discards that card.";
    let parsed = engine::parser::oracle_effect::parse_effect_chain(
        RANDOM,
        engine::types::ability::AbilityKind::Spell,
    );
    let reader = parsed.sub_ability.as_deref().unwrap();
    assert_eq!(
        reader.sub_link,
        engine::types::ability::SubAbilityLink::SequentialSibling
    );
    assert!(matches!(reader.effect.as_ref(), Effect::RevealHand { .. }));
    let mut scene = scenario();
    let (cards, foreign) = color_hand(&mut scene);
    let source = spell(&mut scene, "Random hand SHAPE", RANDOM, black_cost(1));
    let mut runner = scene.build();
    let events = runner.cast(source).target_player(P1).resolve().events;
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::NamedChoice { .. }
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Choose,
                    ..
                }
            ))
            .count(),
        1
    );
    let capture = runner
        .state()
        .active_ability_continuation()
        .unwrap()
        .chain
        .trigger_source
        .clone()
        .unwrap();
    let color = capture
        .lki
        .chosen_attributes
        .iter()
        .rev()
        .find_map(|attribute| match attribute {
            ChosenAttribute::Color(color) => Some(*color),
            _ => None,
        })
        .unwrap();
    let offered: Vec<_> = cards
        .iter()
        .filter(|(_, colors)| colors.contains(&color))
        .map(|(id, _)| *id)
        .collect();
    assert!(offered.len() >= 2);
    prompt(&runner, &offered, false);
    addle_disposal_capture(&runner, source, &capture);
    let chosen = offered[1];
    assert!(runner.state().objects[&chosen].chosen_attributes.is_empty());
    let events = answer(&mut runner, chosen);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::DiscardCard,
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(runner.state().objects[&chosen].zone, Zone::Graveyard);
    unchanged_hand(
        &runner,
        &cards
            .iter()
            .filter(|(id, _)| *id != chosen)
            .map(|(id, _)| *id)
            .chain([foreign])
            .collect::<Vec<_>>(),
    );
    terminal(&runner);
}

#[test]
fn contextual_hand_color_anaphor_crosses_draw_only_in_its_own_body() {
    // SHAPEs: the independent Draw neither carries nor consumes the answer.
    for random in [false, true] {
        let text = if random {
            "Choose a color at random. Draw a card. Target player reveals their hand and you choose a card of that color from it. That player discards that card."
        } else {
            "Choose a color. Draw a card. Target player reveals their hand and you choose a card of that color from it. That player discards that card."
        };
        let mut scene = scenario();
        let (cards, foreign) = color_hand(&mut scene);
        let source = spell(&mut scene, "Draw hand SHAPE", text, black_cost(1));
        let mut runner = scene.build();
        let library = runner.state().players[0].library.clone();
        let mut events = runner.cast(source).target_player(P1).resolve().events;
        if !random {
            color_prompt(&runner);
            let before = runner
                .state()
                .active_ability_continuation()
                .unwrap()
                .chain
                .clone();
            assert!(matches!(before.effect, Effect::Draw { .. }));
            events.extend(
                runner
                    .act(GameAction::ChooseOption {
                        choice: "Blue".into(),
                    })
                    .unwrap()
                    .events,
            );
            // The Draw finished once, while only the downstream hand producer
            // supplied the actual source authority now retained by DiscardCard.
            assert!(before.trigger_source.as_ref().is_none_or(|capture| !capture
                .lki
                .chosen_attributes
                .iter()
                .any(|attribute| matches!(attribute, ChosenAttribute::Color(_)))));
        } else {
            assert!(!matches!(
                runner.state().waiting_for,
                WaitingFor::NamedChoice { .. }
            ));
        }
        let capture = runner
            .state()
            .active_ability_continuation()
            .unwrap()
            .chain
            .trigger_source
            .clone()
            .unwrap();
        let color = capture
            .lki
            .chosen_attributes
            .iter()
            .rev()
            .find_map(|attribute| match attribute {
                ChosenAttribute::Color(color) => Some(*color),
                _ => None,
            })
            .unwrap();
        let offered: Vec<_> = cards
            .iter()
            .filter(|(_, colors)| colors.contains(&color))
            .map(|(id, _)| *id)
            .collect();
        prompt(&runner, &offered, false);
        addle_disposal_capture(&runner, source, &capture);
        assert_eq!(runner.state().players[0].library.len(), library.len() - 1);
        assert_eq!(runner.state().players[0].library.as_slice(), &library[1..]);
        assert_eq!(runner.state().objects[&library[0]].zone, Zone::Hand);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Draw,
                        ..
                    }
                ))
                .count(),
            1
        );
        let selected = offered[1];
        events.extend(answer(&mut runner, selected));
        assert_eq!(runner.state().objects[&selected].zone, Zone::Graveyard);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::DiscardCard,
                        ..
                    }
                ))
                .count(),
            1
        );
        unchanged_hand(
            &runner,
            &cards
                .iter()
                .filter(|(id, _)| *id != selected)
                .map(|(id, _)| *id)
                .chain([foreign])
                .collect::<Vec<_>>(),
        );
        terminal(&runner);
    }
    for initial_random in [false, true] {
        let text = if initial_random {
            "Choose a color at random. Draw a card. Choose a color. Target player reveals their hand and you choose a card of that color from it. That player discards that card."
        } else {
            "Choose a color. Draw a card. Choose a color. Target player reveals their hand and you choose a card of that color from it. That player discards that card."
        };
        let mut scene = scenario();
        let (cards, _) = color_hand(&mut scene);
        let source = spell(&mut scene, "Two choices SHAPE", text, black_cost(1));
        let mut runner = scene.build();
        runner.cast(source).target_player(P1).resolve();
        if !initial_random {
            color_prompt(&runner);
            let tail = &runner.state().active_ability_continuation().unwrap().chain;
            assert!(matches!(tail.effect, Effect::Draw { .. }));
            assert!(matches!(
                tail.sub_ability.as_ref().unwrap().effect,
                Effect::Choose { .. }
            ));
            runner
                .act(GameAction::ChooseOption {
                    choice: "Blue".into(),
                })
                .unwrap();
        }
        let second = color_prompt(&runner);
        assert!(!second
            .lki
            .chosen_attributes
            .iter()
            .any(|attribute| matches!(attribute, ChosenAttribute::Color(_))));
        let reader = &runner.state().active_ability_continuation().unwrap().chain;
        assert!(reader.trigger_source.as_ref().is_none_or(|capture| !capture
            .lki
            .chosen_attributes
            .iter()
            .any(|attribute| matches!(attribute, ChosenAttribute::Color(_)))));
        runner
            .act(GameAction::ChooseOption {
                choice: "Red".into(),
            })
            .unwrap();
        let offered: Vec<_> = cards
            .iter()
            .filter(|(_, colors)| colors.contains(&ManaColor::Red))
            .map(|(id, _)| *id)
            .collect();
        prompt(&runner, &offered, false);
        answer(&mut runner, offered[1]);
        assert_eq!(runner.state().objects[&offered[1]].zone, Zone::Graveyard);
        terminal(&runner);
    }
}

#[test]
fn contextual_hand_addle_ai_both_actions_authorized_controller_and_empty_cohorts() {
    for diverted in [false, true] {
        let mut scene = scenario();
        let (cards, _) = color_hand(&mut scene);
        let source = spell(&mut scene, "Addle", ADDLE, black_cost(1));
        let mut runner = scene.build();
        runner.cast(source).target_player(P1).resolve();
        color_prompt(&runner);
        if diverted {
            runner.state_mut().turn_decision_controller = Some(PlayerId(2));
        }
        let actor = if diverted { PlayerId(2) } else { P0 };
        let selected = cards
            .iter()
            .filter(|(_, colors)| colors.contains(&ManaColor::Blue))
            .nth(1)
            .unwrap()
            .0;
        let mut events = vec![];
        for card_stage in [false, true] {
            let selected = cards
                .iter()
                .filter(|(_, colors)| colors.contains(&ManaColor::Blue))
                .nth(1)
                .unwrap()
                .0;
            let action = engine::ai_support::legal_actions_for_viewer(runner.state(), actor).0.into_iter().find(|action| if card_stage {matches!(action, GameAction::SelectCards {cards} if cards == &vec![selected])} else {matches!(action, GameAction::ChooseOption {choice} if choice == "Blue")}).expect("actual nondefault AI action");
            assert!(
                engine::ai_support::legal_actions_for_viewer(runner.state(), P1)
                    .0
                    .is_empty()
            );
            let mut wrong = runner.state().clone();
            assert!(engine::apply(&mut wrong, P1, action.clone()).is_err());
            events.extend(
                engine::apply(runner.state_mut(), actor, action)
                    .unwrap()
                    .events,
            );
            if !card_stage {
                prompt(
                    &runner,
                    &cards
                        .iter()
                        .filter(|(_, colors)| colors.contains(&ManaColor::Blue))
                        .map(|(id, _)| *id)
                        .collect::<Vec<_>>(),
                    false,
                );
            }
        }
        settle(&mut runner, &mut events);
        assert_eq!(runner.state().objects[&selected].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&selected].owner, P1);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::DiscardCard,
                        ..
                    }
                ))
                .count(),
            1
        );
        unchanged_hand(
            &runner,
            &cards
                .iter()
                .filter(|(id, _)| *id != selected)
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
        );
        assert!(runner.state().objects[&source].chosen_attributes.is_empty());
        terminal(&runner);
    }
    for empty in [false, true] {
        let mut scene = scenario();
        let decoy = (!empty).then(|| {
            scene
                .add_spell_to_hand(P1, "Red only", false)
                .with_color(vec![ManaColor::Red])
                .id()
        });
        let source = spell(&mut scene, "Addle", ADDLE, black_cost(1));
        let mut runner = scene.build();
        runner.cast(source).target_player(P1).resolve();
        color_prompt(&runner);
        runner
            .act(GameAction::ChooseOption {
                choice: "Blue".into(),
            })
            .unwrap();
        let mut events = vec![];
        settle(&mut runner, &mut events);
        if let Some(decoy) = decoy {
            unchanged_hand(&runner, &[decoy]);
        }
        terminal(&runner);
    }
}

#[test]
fn contextual_hand_optional_empty_color_custody_public_decline() {
    // SHAPE with explicit 1B cost; the contextual optional-empty route still parks its owner.
    let mut scene = scenario();
    let decoy = scene
        .add_spell_to_hand(P1, "Red optional decoy", true)
        .with_color(vec![ManaColor::Red])
        .id();
    let source = spell(&mut scene, "Optional color SHAPE", "Choose a color. Target player reveals their hand and you may choose a card of that color from it. That player discards that card.", black_cost(1));
    let mut runner = scene.build();
    runner.cast(source).target_player(P1).resolve();
    let mut expected = color_prompt(&runner);
    expected
        .lki
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Blue));
    runner
        .act(GameAction::ChooseOption {
            choice: "Blue".into(),
        })
        .unwrap();
    prompt(&runner, &[], true);
    addle_disposal_capture(&runner, source, &expected);
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .unwrap();
    let mut events = vec![];
    settle(&mut runner, &mut events);
    unchanged_hand(&runner, &[decoy]);
    terminal(&runner);
}

#[test]
fn contextual_hand_two_paid_addles_keep_source_controller_hand_and_color_separate() {
    let mut scene = scenario();
    let (first_hand, _) = color_hand(&mut scene);
    let red_first = scene
        .add_spell_to_hand(PlayerId(2), "Second hand red first", true)
        .with_color(vec![ManaColor::Red])
        .id();
    let red_second = scene
        .add_land_to_hand(PlayerId(2), "Second hand red land")
        .with_color(vec![ManaColor::Red])
        .id();
    let red_decoy = scene
        .add_land_to_hand(PlayerId(2), "Second hand colorless")
        .id();
    let first_source = spell(&mut scene, "Addle", ADDLE, black_cost(1));
    let second_source = scene
        .add_spell_to_hand_from_oracle(P1, "Addle", false, ADDLE)
        .with_mana_cost(black_cost(1))
        .id();
    let mana_one = scene.add_basic_land(P1, ManaColor::Black);
    let mana_two = scene.add_basic_land(P1, ManaColor::Black);
    let mut runner = scene.build();
    runner.cast(first_source).target_player(P1).resolve();
    color_prompt(&runner);
    runner
        .act(GameAction::ChooseOption {
            choice: "Blue".into(),
        })
        .unwrap();
    let blue: Vec<_> = first_hand
        .iter()
        .filter(|(_, colors)| colors.contains(&ManaColor::Blue))
        .map(|(id, _)| *id)
        .collect();
    prompt(&runner, &blue, false);
    let first_capture = runner
        .state()
        .active_ability_continuation()
        .unwrap()
        .chain
        .trigger_source
        .clone()
        .unwrap();
    answer(&mut runner, blue[1]);
    terminal(&runner);
    runner.advance_to_combat();
    runner.declare_attackers(&[]).unwrap();
    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    runner.auto_advance_to_main_phase();
    assert_eq!(runner.state().active_player, P1);
    runner.activate(mana_one, 0).resolve();
    runner.activate(mana_two, 0).resolve();
    runner
        .cast(second_source)
        .target_player(PlayerId(2))
        .resolve();
    let WaitingFor::NamedChoice {
        player,
        options,
        source: Some(source),
        ..
    } = &runner.state().waiting_for
    else {
        panic!("second paid color prompt");
    };
    assert_eq!(*player, P1);
    assert_eq!(options, &["White", "Blue", "Black", "Red", "Green"]);
    assert_eq!(source.prompt.identity.reference.object_id, second_source);
    assert_ne!(source.context.as_ref(), Some(&first_capture));
    runner
        .act(GameAction::ChooseOption {
            choice: "Red".into(),
        })
        .unwrap();
    let WaitingFor::RevealChoice { player, cards, .. } = &runner.state().waiting_for else {
        panic!("second real hand prompt");
    };
    assert_eq!(*player, P1);
    assert_eq!(cards, &[red_first, red_second]);
    let pending = runner.state().active_ability_continuation().unwrap();
    assert_eq!(pending.chain.source_id, second_source);
    assert_eq!(pending.chain.controller, P1);
    assert!(pending
        .chain
        .trigger_source
        .as_ref()
        .unwrap()
        .lki
        .chosen_attributes
        .iter()
        .any(|a| *a == ChosenAttribute::Color(ManaColor::Red)));
    assert!(runner.state().objects[&red_second]
        .chosen_attributes
        .is_empty());
    runner
        .act(GameAction::SelectCards {
            cards: vec![red_second],
        })
        .unwrap();
    let mut events = vec![];
    settle(&mut runner, &mut events);
    assert_eq!(runner.state().objects[&red_second].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&red_second].owner, PlayerId(2));
    assert_eq!(runner.state().objects[&blue[1]].zone, Zone::Graveyard);
    unchanged_hand(&runner, &[blue[0], red_first, red_decoy]);
    assert!(
        runner.state().stack.is_empty()
            && runner.state().trigger_stack.is_empty()
            && runner.state().resolution_stack.is_empty()
    );
    assert!(runner.state().owed_spell_final_part().is_none());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn contextual_hand_paid_spells_exact_nondefault_selection_and_public_rejections() {
    // CR 109.2a + CR 701.9b: every restriction applies to the card chosen by the spell controller.
    for (name, text, cost, lay_bare) in [
        ("Duress", DURESS, black_cost(0), false),
        ("Lay Bare the Heart", LAY_BARE, black_cost(1), true),
        (
            "Mardu Charm",
            MARDU,
            ManaCost::Cost {
                generic: 0,
                shards: vec![
                    ManaCostShard::Red,
                    ManaCostShard::White,
                    ManaCostShard::Black,
                ],
            },
            false,
        ),
    ] {
        let mut scene = scenario();
        let ids = hand(&mut scene);
        let source = spell(&mut scene, name, text, cost);
        let mut runner = scene.build();
        let outcome = if name == "Mardu Charm" {
            runner.cast(source).modes(&[2]).target_player(P1).resolve()
        } else {
            runner.cast(source).target_player(P1).resolve()
        };
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::RevealChoice { .. }
        ));
        let mut offered = vec![ids.first, ids.second];
        if lay_bare {
            offered.push(ids.creature);
        }
        prompt(&runner, &offered, false);
        let pending = serde_json::to_value(runner.state()).unwrap();
        for (actor, action) in [
            (
                P1,
                GameAction::SelectCards {
                    cards: vec![ids.second],
                },
            ),
            (P0, GameAction::SelectCards { cards: vec![] }),
            (
                P0,
                GameAction::SelectCards {
                    cards: vec![ids.first, ids.second],
                },
            ),
            (
                P0,
                GameAction::SelectCards {
                    cards: vec![ids.land],
                },
            ),
            (
                P0,
                GameAction::SelectCards {
                    cards: vec![ids.foreign],
                },
            ),
            (
                P0,
                GameAction::CastSpell {
                    object_id: ids.first,
                    card_id: runner.state().objects[&ids.first].card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                },
            ),
        ] {
            let mut attempted = runner.state().clone();
            assert!(engine::apply(&mut attempted, actor, action).is_err());
            assert_eq!(serde_json::to_value(runner.state()).unwrap(), pending);
        }
        let events = answer(&mut runner, ids.second);
        assert_eq!(runner.state().objects[&ids.second].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&ids.second].owner, P1);
        unchanged_hand(
            &runner,
            &[
                ids.first,
                ids.creature,
                ids.legendary,
                ids.land,
                ids.foreign,
            ],
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Discard,
                        ..
                    }
                ))
                .count(),
            1
        );
        terminal(&runner);
    }
}

#[test]
fn contextual_hand_thoughtseize_empty_ineligible_and_selected_tail_once() {
    // CR 608.2c: the independent final life instruction still happens without an eligible card.
    for population in [0, 1, 2] {
        let mut scene = scenario();
        let source = spell(&mut scene, "Thoughtseize", THOUGHTSEIZE, black_cost(0));
        let land = (population > 0).then(|| scene.add_land_to_hand(P1, "Only land").id());
        let eligible = if population == 2 {
            vec![
                scene.add_creature_to_hand(P1, "First creature", 1, 1).id(),
                scene.add_spell_to_hand(P1, "Second spell", false).id(),
            ]
        } else {
            vec![]
        };
        let mut runner = scene.build();
        let mut events = runner
            .cast(source)
            .target_player(P1)
            .resolve()
            .events()
            .to_vec();
        if population == 2 {
            prompt(&runner, &eligible, false);
            events.extend(answer(&mut runner, eligible[1]));
            assert_eq!(runner.state().objects[&eligible[1]].zone, Zone::Graveyard);
            unchanged_hand(&runner, &[eligible[0]]);
        }
        if let Some(land) = land {
            unchanged_hand(&runner, &[land]);
        }
        assert_eq!(runner.state().players[0].life, 18);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::LifeChanged {
                        player_id: P0,
                        amount: -2,
                        ..
                    }
                ))
                .count(),
            1
        );
        terminal(&runner);
    }
}

#[test]
fn contextual_hand_dread_fugue_real_base_and_cleave_costs() {
    // CR 702.148a: the paid alternate cost removes the bracketed MV restriction, not printed MV.
    for cleave in [false, true] {
        let mut scene = scenario();
        let ids = hand(&mut scene);
        let source = spell(&mut scene, "Dread Fugue", DREAD, black_cost(0));
        let mut runner = scene.build();
        let cast = runner.cast(source).target_player(P1);
        if cleave {
            cast.casting_variant(CastingVariant::Cleave).resolve();
        } else {
            cast.casting_variant(CastingVariant::Normal).resolve();
        }
        let mut offered = vec![ids.first, ids.creature, ids.legendary];
        if cleave {
            offered.push(ids.second);
        }
        prompt(&runner, &offered, false);
        let chosen = if cleave { ids.second } else { ids.first };
        answer(&mut runner, chosen);
        assert_eq!(runner.state().objects[&chosen].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&source].mana_cost, black_cost(0));
        unchanged_hand(&runner, &[ids.land, ids.foreign]);
        terminal(&runner);
    }
}

pub(super) fn restored_forms(state: &GameState) -> Vec<GameState> {
    fn historic_fields(value: &mut Value) {
        match value {
            Value::Object(fields) => {
                // These fields did not exist in the historical hand-continuation writers.
                fields.remove("delivery_owner");
                for child in fields.values_mut() {
                    historic_fields(child);
                }
            }
            Value::Array(values) => {
                for child in values {
                    historic_fields(child);
                }
            }
            _ => {}
        }
    }
    let raw = serde_json::to_value(state).unwrap();
    let mut forms = vec![
        serde_json::from_value(raw.clone()).unwrap(),
        serde_json::from_value::<PersistedGameState>(raw.clone())
            .unwrap()
            .into_game_state()
            .unwrap(),
        serde_json::from_value::<PersistedGameState>(
            serde_json::to_value(PersistedGameState::capture(state.clone())).unwrap(),
        )
        .unwrap()
        .into_game_state()
        .unwrap(),
    ];
    let mut v1 = raw;
    v1.as_object_mut().unwrap().remove("resolution_stack");
    v1["resolution_state_version"] = json!(1);
    if let Some(continuation) = state.active_ability_continuation() {
        v1["pending_continuation"] = serde_json::to_value(continuation).unwrap();
    }
    if let Some(spell) = state.active_spell_resolution() {
        v1["pending_spell_resolution"] = serde_json::to_value(spell).unwrap();
    }
    historic_fields(&mut v1);
    forms.push(
        serde_json::from_value::<ResolutionStateWire>(v1)
            .unwrap()
            .into_game_state(),
    );
    for version in [2, 3, 4] {
        let mut wire =
            serde_json::to_value(ResolutionStateWire::from_game_state(state.clone())).unwrap();
        historic_fields(&mut wire);
        wire["resolution_state_version"] = json!(version);
        forms.push(
            serde_json::from_value::<ResolutionStateWire>(wire)
                .unwrap()
                .into_game_state(),
        );
    }
    for form in &forms {
        assert_eq!(form.waiting_for, state.waiting_for);
        assert_eq!(form.owed_spell_final_part(), state.owed_spell_final_part());
    }
    forms
}

#[test]
fn contextual_hand_current_raw_trusted_authentic_v1_v4_resume_nondefault() {
    let mut scene = scenario();
    let ids = hand(&mut scene);
    let source = spell(&mut scene, "Thoughtseize", THOUGHTSEIZE, black_cost(0));
    let mut runner = scene.build();
    runner.cast(source).target_player(P1).resolve();
    let offered = [ids.first, ids.second, ids.creature, ids.legendary];
    prompt(&runner, &offered, false);
    for state in restored_forms(runner.state()) {
        let mut restored = GameRunner::from_state(state);
        prompt(&restored, &offered, false);
        answer(&mut restored, ids.second);
        assert_eq!(restored.state().objects[&ids.second].zone, Zone::Graveyard);
        unchanged_hand(
            &restored,
            &[
                ids.first,
                ids.creature,
                ids.legendary,
                ids.land,
                ids.foreign,
            ],
        );
        assert_eq!(restored.state().players[0].life, 18);
        terminal(&restored);
    }
}

#[test]
fn contextual_hand_actual_ai_and_authorized_turn_controller() {
    let mut scene = scenario();
    let ids = hand(&mut scene);
    let source = spell(&mut scene, "Duress", DURESS, black_cost(0));
    let mut runner = scene.build();
    runner.cast(source).target_player(P1).resolve();
    prompt(&runner, &[ids.first, ids.second], false);
    runner.state_mut().turn_decision_controller = Some(PlayerId(2));
    let actions = engine::ai_support::legal_actions_for_viewer(runner.state(), PlayerId(2));
    let action = actions.0.into_iter().find(|action| matches!(action, GameAction::SelectCards { cards } if cards == &vec![ids.second])).expect("AI enumerates the actual nondefault public response");
    assert!(
        engine::ai_support::legal_actions_for_viewer(runner.state(), P0)
            .0
            .is_empty()
    );
    let mut unauthorized = runner.state().clone();
    assert!(engine::apply(&mut unauthorized, P0, action.clone()).is_err());
    engine::apply(runner.state_mut(), PlayerId(2), action).unwrap();
    let mut events = vec![];
    settle(&mut runner, &mut events);
    assert_eq!(runner.state().objects[&ids.second].zone, Zone::Graveyard);
    unchanged_hand(&runner, &[ids.first, ids.creature, ids.land, ids.foreign]);
    terminal(&runner);
}

fn envelopes(state: &GameState, viewer: Option<PlayerId>) -> [Value; 2] {
    let before = serde_json::to_value(state).unwrap();
    let filtered = viewer.map_or_else(
        || filter_state_for_unseated_viewer(state),
        |viewer| filter_state_for_viewer(state, viewer),
    );
    assert_eq!(
        serde_json::to_value(state).unwrap(),
        before,
        "filtering is pure"
    );
    let wrapped = serde_json::to_value(ClientGameStateRef::wrap(state, viewer)).unwrap();
    assert_eq!(serde_json::to_value(state).unwrap(), before, "wrap is pure");
    let wrapped_filtered =
        serde_json::to_value(ClientGameStateRef::wrap_filtered(state, &filtered, viewer)).unwrap();
    let result = [wrapped, wrapped_filtered];
    assert_eq!(
        serde_json::to_value(state).unwrap(),
        before,
        "both projections are pure"
    );
    result
}

#[test]
fn contextual_hand_both_envelopes_public_reveal_private_look_and_private_decoys() {
    // CR 701.20a/e: a public reveal and private look have distinct audiences.
    for private in [false, true] {
        let mut scene = scenario();
        let ids = hand(&mut scene);
        let text = if private {
            "Look at target player's hand. You choose a nonland card from it. That player discards that card."
        } else {
            DURESS
        };
        let source = spell(
            &mut scene,
            if private {
                "SHAPE private look"
            } else {
                "Duress"
            },
            text,
            black_cost(0),
        );
        let mut runner = scene.build();
        let libraries: Vec<_> = runner
            .state()
            .players
            .iter()
            .map(|player| player.library.clone())
            .collect();
        for viewer in [Some(P0), Some(P1), Some(PlayerId(2)), None] {
            for wire in envelopes(runner.state(), viewer) {
                assert!(wire["state"].is_object());
                let printed = wire.to_string();
                assert_eq!(printed.contains("Hand second MV3"), viewer == Some(P1));
                assert_eq!(
                    printed.contains("Other player's secret"),
                    viewer == Some(PlayerId(2))
                );
            }
        }
        runner.cast(source).target_player(P1).resolve();
        let offered = if private {
            vec![ids.first, ids.second, ids.creature, ids.legendary]
        } else {
            vec![ids.first, ids.second]
        };
        prompt(&runner, &offered, false);
        let authority = serde_json::to_value(runner.state()).unwrap();
        for viewer in [Some(P0), Some(P1), Some(PlayerId(2)), None] {
            for wire in envelopes(runner.state(), viewer) {
                assert!(wire["state"].is_object());
                let visible_options = if !private || matches!(viewer, Some(P0) | Some(P1)) {
                    offered.clone()
                } else {
                    vec![]
                };
                assert_eq!(
                    wire["state"]["waiting_for"]["data"]["cards"],
                    json!(visible_options)
                );
                assert_eq!(wire["state"]["waiting_for"]["data"]["player"], json!(P0));
                assert_eq!(wire["derived"]["unique_authorized_submitter"], json!(P0));
                if private && viewer != Some(P0) {
                    assert!(wire["state"].get("private_look_ids").is_none());
                    assert!(wire["state"].get("private_look_player").is_none());
                }
                let printed = wire.to_string();
                assert_eq!(
                    printed.contains("Hand second MV3"),
                    !private || viewer == Some(P0) || viewer == Some(P1)
                );
                assert_eq!(
                    printed.contains("Hand land"),
                    !private || matches!(viewer, Some(P0) | Some(P1))
                );
                assert_eq!(
                    printed.contains("Other player's secret"),
                    viewer == Some(PlayerId(2))
                );
                assert!(!printed.contains("Private library one"));
                assert!(
                    serde_json::from_value::<PersistedGameState>(wire["state"].clone()).is_err(),
                    "viewer envelope cannot become authoritative persistence"
                );
            }
        }
        assert_eq!(serde_json::to_value(runner.state()).unwrap(), authority);
        // Canonical raw/trusted saves of the very same captured authority remain
        // answerable; viewer copies above are refused by canonical ingress.
        for state in restored_forms(runner.state()) {
            let mut restored = GameRunner::from_state(state);
            prompt(&restored, &offered, false);
            answer(&mut restored, ids.second);
            assert_eq!(restored.state().objects[&ids.second].zone, Zone::Graveyard);
            terminal(&restored);
        }
        let events = answer(&mut runner, ids.second);
        assert_eq!(
            runner
                .state()
                .players
                .iter()
                .map(|player| player.library.clone())
                .collect::<Vec<_>>(),
            libraries
        );
        assert_eq!(runner.state().objects[&ids.second].zone, Zone::Graveyard);
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: Zone::Hand, to: Zone::Graveyard, .. } if *object_id == ids.second)).count(), 1);
        unchanged_hand(
            &runner,
            &[
                ids.first,
                ids.creature,
                ids.legendary,
                ids.land,
                ids.foreign,
            ],
        );
        assert!(runner.state().private_look_ids.is_empty());
        assert_eq!(runner.state().private_look_player, None);
        terminal(&runner);
        for viewer in [Some(P0), Some(P1), Some(PlayerId(2)), None] {
            for wire in envelopes(runner.state(), viewer) {
                assert!(wire["state"].is_object());
                assert_eq!(wire["state"]["waiting_for"]["type"], json!("Priority"));
                let printed = wire.to_string();
                assert!(
                    printed.contains("Hand second MV3"),
                    "chosen graveyard card is public"
                );
                assert_eq!(
                    printed.contains("Hand first MV2"),
                    !private || matches!(viewer, Some(P0) | Some(P1))
                );
                assert_eq!(
                    printed.contains("Other player's secret"),
                    viewer == Some(PlayerId(2))
                );
                assert!(!printed.contains("Private library one"));
            }
        }
    }
}

#[test]
fn contextual_hand_private_optional_zero_one_four_cohorts_and_known_candidates() {
    // SHAPE private optional look: every cohort has five cards, so neither
    // private eligible membership nor its cardinality is public information.
    for count in [0, 1, 4] {
        for controlled in [false, true] {
            for decline in [false, true] {
                for public_first in [false, true] {
                    if public_first && count == 0 {
                        continue;
                    }
                    let mut scene = scenario();
                    let mut eligible = vec![];
                    let mut all = vec![];
                    for index in 0..5 {
                        let name = format!("Cohort private {index}");
                        let id = if index < count {
                            scene.add_spell_to_hand(P1, &name, false).id()
                        } else {
                            scene.add_land_to_hand(P1, &name).id()
                        };
                        all.push(id);
                        if index < count {
                            eligible.push(id);
                        }
                    }
                    let foreign = scene
                        .add_spell_to_hand(PlayerId(2), "Unrelated private hand", false)
                        .id();
                    let source = spell(&mut scene, "SHAPE optional private cohort", "Look at target player's hand. You may choose a nonland card from it. That player discards that card.", black_cost(0));
                    let mut runner = scene.build();
                    // Initialized public identity is a lawful known-card positive;
                    // no choice, continuation, result or event is manufactured.
                    if public_first {
                        runner
                            .state_mut()
                            .resolve_and_apply_information(
                                &[eligible[0]],
                                ResolvedInformationAudience::Public,
                                ResolvedInformationLifetime::UntilZoneChange,
                                ResolvedInformationEdit::Reveal,
                            )
                            .unwrap();
                    }
                    runner.cast(source).target_player(P1).resolve();
                    prompt(&runner, &eligible, true);
                    if controlled {
                        runner.state_mut().turn_decision_controller = Some(PlayerId(2));
                    }
                    let submitter = if controlled { PlayerId(2) } else { P0 };
                    let before = serde_json::to_value(runner.state()).unwrap();
                    let libraries: Vec<_> = runner
                        .state()
                        .players
                        .iter()
                        .map(|player| player.library.clone())
                        .collect();
                    for viewer in [Some(P0), Some(P1), Some(PlayerId(2)), None] {
                        for wire in envelopes(runner.state(), viewer) {
                            assert!(wire["state"].is_object());
                            let entitled = matches!(viewer, Some(P0) | Some(P1))
                                || (controlled && viewer == Some(PlayerId(2)));
                            let expected = if entitled {
                                eligible.clone()
                            } else if public_first {
                                vec![eligible[0]]
                            } else {
                                vec![]
                            };
                            assert_eq!(
                                wire["state"]["waiting_for"]["data"]["cards"],
                                json!(expected)
                            );
                            assert_eq!(wire["state"]["waiting_for"]["data"]["player"], json!(P0));
                            assert_eq!(
                                wire["derived"]["unique_authorized_submitter"],
                                json!(submitter)
                            );
                            if viewer != Some(P0) && !(controlled && viewer == Some(PlayerId(2))) {
                                assert!(wire["state"].get("private_look_ids").is_none());
                                assert!(wire["state"].get("private_look_player").is_none());
                            }
                            let printed = wire.to_string();
                            for (index, id) in all.iter().enumerate() {
                                assert_eq!(
                                    printed.contains(&format!("Cohort private {index}")),
                                    entitled || (public_first && *id == eligible[0])
                                );
                            }
                        }
                    }
                    assert_eq!(serde_json::to_value(runner.state()).unwrap(), before);
                    let selected = if decline || eligible.is_empty() {
                        vec![]
                    } else {
                        vec![*eligible.last().unwrap()]
                    };
                    for actor in [P0, P1, PlayerId(2)]
                        .into_iter()
                        .filter(|actor| *actor != submitter)
                    {
                        let mut rejected = runner.state().clone();
                        assert!(engine::apply(
                            &mut rejected,
                            actor,
                            GameAction::SelectCards {
                                cards: selected.clone()
                            }
                        )
                        .is_err());
                    }
                    let result = engine::apply(
                        runner.state_mut(),
                        submitter,
                        GameAction::SelectCards {
                            cards: selected.clone(),
                        },
                    )
                    .unwrap();
                    let mut events = result.events;
                    settle(&mut runner, &mut events);
                    assert_eq!(
                        runner
                            .state()
                            .players
                            .iter()
                            .map(|player| player.library.clone())
                            .collect::<Vec<_>>(),
                        libraries
                    );
                    for id in &all {
                        assert_eq!(
                            runner.state().objects[id].zone,
                            if selected.contains(id) {
                                Zone::Graveyard
                            } else {
                                Zone::Hand
                            }
                        );
                    }
                    unchanged_hand(&runner, &[foreign]);
                    assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: Zone::Hand, to: Zone::Graveyard, .. } if selected.contains(object_id))).count(), selected.len());
                    assert!(runner.state().private_look_ids.is_empty());
                    assert_eq!(runner.state().private_look_player, None);
                    terminal(&runner);
                    for viewer in [Some(P0), Some(P1), Some(PlayerId(2)), None] {
                        for wire in envelopes(runner.state(), viewer) {
                            assert!(wire["state"].is_object());
                            assert_eq!(wire["state"]["waiting_for"]["type"], json!("Priority"));
                            let printed = wire.to_string();
                            for (index, id) in all.iter().enumerate() {
                                let visible = selected.contains(id)
                                    || matches!(viewer, Some(P0) | Some(P1))
                                    || (public_first && *id == eligible[0]);
                                assert_eq!(
                                    printed.contains(&format!("Cohort private {index}")),
                                    visible
                                );
                            }
                            assert_eq!(
                                printed.contains("Unrelated private hand"),
                                viewer == Some(PlayerId(2))
                            );
                            assert!(!printed.contains("Private library one"));
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn contextual_hand_mardu_damage_and_token_modes_keep_their_whole_instructions() {
    // CR 601.2b + CR 608.2c: selecting another printed mode does not execute the hand mode.
    for mode in [0, 1] {
        let mut scene = scenario();
        let ids = hand(&mut scene);
        let victim = scene.add_creature(P1, "Damage target", 3, 5).id();
        let source = spell(
            &mut scene,
            "Mardu Charm",
            MARDU,
            ManaCost::Cost {
                generic: 0,
                shards: vec![
                    ManaCostShard::Red,
                    ManaCostShard::White,
                    ManaCostShard::Black,
                ],
            },
        );
        let mut runner = scene.build();
        if mode == 0 {
            runner
                .cast(source)
                .modes(&[0])
                .target_object(victim)
                .resolve();
        } else {
            runner.cast(source).modes(&[1]).resolve();
        }
        unchanged_hand(
            &runner,
            &[
                ids.first,
                ids.second,
                ids.creature,
                ids.legendary,
                ids.land,
                ids.foreign,
            ],
        );
        if mode == 0 {
            assert_eq!(runner.state().objects[&victim].damage_marked, 4);
            assert_eq!(runner.state().objects[&victim].zone, Zone::Battlefield);
        } else {
            let tokens: Vec<_> = runner
                .state()
                .objects
                .iter()
                .filter(|(_, object)| {
                    object.is_token && object.zone == Zone::Battlefield && object.controller == P0
                })
                .map(|(id, _)| *id)
                .collect();
            assert_eq!(tokens.len(), 2);
            for id in &tokens {
                let object = &runner.state().objects[id];
                assert_eq!((object.power, object.toughness), (Some(1), Some(1)));
                assert_eq!(object.card_types.subtypes, vec!["Warrior"]);
                assert_eq!(object.color, vec![engine::types::mana::ManaColor::White]);
                assert!(object.has_keyword(&Keyword::FirstStrike));
            }
            // CR 514.2: the printed first-strike grant expires at actual cleanup.
            runner.advance_to_combat();
            runner.declare_attackers(&[]).unwrap();
            runner.advance_to_end_step();
            runner.advance_to_upkeep();
            for id in tokens {
                assert!(!runner.state().objects[&id].has_keyword(&Keyword::FirstStrike));
            }
        }
        assert!(runner.state().stack.is_empty());
        assert!(runner.state().resolution_stack.is_empty());
    }
}

#[test]
fn contextual_hand_nightmare_natural_three_chapters_and_final_sacrifice() {
    let mut scene = scenario();
    let ids = hand(&mut scene);
    let victim = scene
        .add_creature(P1, "Power two chapter target", 2, 2)
        .id();
    let large = scene.add_creature(P1, "Power three decoy", 3, 3).id();
    let own = scene.add_creature(P0, "Own small decoy", 1, 1).id();
    // The permanent type and Saga subtype precede parsing/synthesis.
    let source = scene
        .add_spell_to_hand(P0, "Elspeth's Nightmare", false)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .with_mana_cost(black_cost(2))
        .from_oracle_text(NIGHTMARE)
        .id();
    let mut runner = scene.build();
    runner.cast(source).target_object(victim).resolve();
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&large].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&own].zone, Zone::Battlefield);
    for chapter in [2, 3] {
        // CR 714.3c: real turn progression adds the next lore counter.
        for _ in 0..3 {
            runner.advance_to_combat();
            runner.declare_attackers(&[]).unwrap();
            runner.advance_to_end_step();
            runner.advance_to_upkeep();
            runner.auto_advance_to_main_phase();
            if matches!(
                runner.state().waiting_for,
                WaitingFor::TriggerTargetSelection { .. }
            ) {
                break;
            }
        }
        let WaitingFor::TriggerTargetSelection {
            source_id,
            player,
            target_slots,
            selection,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "natural chapter {chapter} target prompt: {:?}",
                runner.state().waiting_for
            )
        };
        assert_eq!(*source_id, Some(source));
        assert_eq!(*player, P0);
        assert!(target_slots[selection.current_slot]
            .legal_targets
            .contains(&TargetRef::Player(P1)));
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            })
            .unwrap();
        let mut events = vec![];
        settle(&mut runner, &mut events);
        if chapter == 2 {
            prompt(&runner, &[ids.first, ids.second], false);
            answer(&mut runner, ids.second);
            assert_eq!(runner.state().objects[&ids.second].zone, Zone::Graveyard);
            unchanged_hand(&runner, &[ids.first, ids.creature, ids.land, ids.foreign]);
        } else {
            assert_eq!(runner.state().objects[&victim].zone, Zone::Exile);
            assert_eq!(runner.state().objects[&ids.second].zone, Zone::Exile);
            // CR 704.5s: sacrifice follows the final chapter leaving the stack.
            assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        }
        terminal(&runner);
    }
}

#[test]
fn contextual_hand_optional_decline_and_two_reveals_do_not_reuse_selected_card() {
    // SHAPE controls use the same real producer/action/continuation as the printed cards.
    for decline in [false, true] {
        let mut scene = scenario();
        let ids = hand(&mut scene);
        let text = "Target opponent reveals their hand. You may choose a nonland card from it. That player discards that card.";
        let source = spell(
            &mut scene,
            "SHAPE optional hand choice",
            text,
            black_cost(0),
        );
        let mut runner = scene.build();
        runner.cast(source).target_player(P1).resolve();
        prompt(
            &runner,
            &[ids.first, ids.second, ids.creature, ids.legendary],
            true,
        );
        runner
            .act(GameAction::SelectCards {
                cards: if decline { vec![] } else { vec![ids.second] },
            })
            .unwrap();
        let mut events = vec![];
        settle(&mut runner, &mut events);
        assert_eq!(
            runner.state().objects[&ids.second].zone,
            if decline { Zone::Hand } else { Zone::Graveyard }
        );
        unchanged_hand(&runner, &[ids.first, ids.creature, ids.land, ids.foreign]);
        terminal(&runner);
    }
    let mut scene = scenario();
    let ids = hand(&mut scene);
    let source = spell(&mut scene, "SHAPE two independent hand producers", "Target opponent reveals their hand. You choose a noncreature, nonland card from it. That player discards that card. Target opponent reveals their hand. You choose a creature card from it. That player discards that card.", black_cost(0));
    let mut runner = scene.build();
    runner.cast(source).target_player(P1).resolve();
    prompt(&runner, &[ids.first, ids.second], false);
    answer(&mut runner, ids.second);
    prompt(&runner, &[ids.creature, ids.legendary], false);
    let mut attempted = runner.state().clone();
    assert!(engine::apply(
        &mut attempted,
        P0,
        GameAction::SelectCards {
            cards: vec![ids.second]
        }
    )
    .is_err());
    answer(&mut runner, ids.legendary);
    assert_eq!(runner.state().objects[&ids.second].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&ids.legendary].zone, Zone::Graveyard);
    unchanged_hand(&runner, &[ids.first, ids.creature, ids.land, ids.foreign]);
    terminal(&runner);
}

#[test]
fn contextual_hand_two_natural_sagas_departure_keeps_independent_chapter_choices() {
    let mut scene = scenario();
    scene.add_basic_land(P0, engine::types::mana::ManaColor::Black);
    scene.with_mana_pool(
        P0,
        (0..8)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let first_hand = hand(&mut scene);
    let second_first = scene
        .add_spell_to_hand(PlayerId(2), "Second Saga first eligible", false)
        .id();
    let second_last = scene
        .add_spell_to_hand(PlayerId(2), "Second Saga nondefault eligible", true)
        .id();
    let second_land = scene
        .add_land_to_hand(PlayerId(2), "Second Saga land decoy")
        .id();
    let second_creature = scene
        .add_creature_to_hand(PlayerId(2), "Second Saga creature decoy", 2, 2)
        .id();
    let first_victim = scene.add_creature(P1, "First chapter I target", 2, 2).id();
    let second_victim = scene
        .add_creature(PlayerId(2), "Second chapter I target", 1, 2)
        .id();
    let large = scene.add_creature(P1, "Large chapter I decoy", 3, 3).id();
    let own = scene.add_creature(P0, "Own chapter I decoy", 1, 1).id();
    let mut sagas = vec![];
    for _ in 0..2 {
        sagas.push(
            scene
                .add_spell_to_hand(P0, "Elspeth's Nightmare", false)
                .as_enchantment()
                .with_subtypes(vec!["Saga"])
                .with_mana_cost(black_cost(2))
                .from_oracle_text(NIGHTMARE)
                .id(),
        );
    }
    // SHAPE response control: ordinary paid instant cast, not a zone mutation
    // during the selection prompt.
    let bounce = scene
        .add_spell_to_hand_from_oracle(
            P0,
            "SHAPE return Saga response",
            true,
            "Return target enchantment to its owner's hand.",
        )
        .with_mana_cost(black_cost(0))
        .id();
    let mut runner = scene.build();
    runner.cast(sagas[0]).target_object(first_victim).resolve();
    terminal(&runner);
    runner.cast(sagas[1]).target_object(second_victim).resolve();
    terminal(&runner);
    assert_eq!(runner.state().objects[&first_victim].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&second_victim].zone, Zone::Graveyard);
    for _ in 0..3 {
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.advance_to_upkeep();
        runner.auto_advance_to_main_phase();
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::OrderTriggers { .. }
        ) {
            break;
        }
    }
    let mut targeted = vec![];
    for _ in 0..12 {
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { player, triggers } => {
                assert_eq!(*player, P0);
                assert_eq!(triggers.len(), 2);
                assert!(triggers
                    .iter()
                    .all(|trigger| sagas.contains(&trigger.source_id)));
                let bottom = triggers
                    .iter()
                    .position(|trigger| trigger.source_id == sagas[1])
                    .unwrap();
                let top = triggers
                    .iter()
                    .position(|trigger| trigger.source_id == sagas[0])
                    .unwrap();
                runner
                    .act(GameAction::OrderTriggers {
                        order: vec![bottom, top],
                    })
                    .unwrap();
            }
            WaitingFor::TriggerTargetSelection {
                player,
                source_id,
                target_slots,
                selection,
                ..
            } => {
                assert_eq!(*player, P0);
                let source = source_id.expect("natural Saga chapter source");
                assert!(sagas.contains(&source));
                assert!(!targeted.contains(&source));
                let target = if source == sagas[0] { P1 } else { PlayerId(2) };
                assert!(target_slots[selection.current_slot]
                    .legal_targets
                    .contains(&TargetRef::Player(target)));
                targeted.push(source);
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(target)),
                    })
                    .unwrap();
            }
            WaitingFor::Priority { .. }
                if targeted.len() == 2 && runner.state().stack.len() == 2 =>
            {
                break
            }
            other => panic!("two natural II target/order windows: {other:?}"),
        }
    }
    targeted.sort();
    let mut expected_sources = sagas.clone();
    expected_sources.sort();
    assert_eq!(targeted, expected_sources);
    assert_eq!(runner.state().stack.len(), 2);
    assert_eq!(runner.state().stack.last().unwrap().source_id, sagas[0]);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    // CR 113.7a: the chapter II trigger is independent of the departed Saga.
    runner.cast(bounce).target_object(sagas[0]).resolve();
    assert_eq!(runner.state().objects[&sagas[0]].zone, Zone::Hand);
    prompt(&runner, &[first_hand.first, first_hand.second], false);
    let mut events = answer(&mut runner, first_hand.second);
    prompt(
        &runner,
        &[first_hand.foreign, second_first, second_last],
        false,
    );
    let mut rejected = runner.state().clone();
    assert!(engine::apply(
        &mut rejected,
        P0,
        GameAction::SelectCards {
            cards: vec![first_hand.second]
        }
    )
    .is_err());
    events.extend(answer(&mut runner, second_last));
    for id in [first_hand.second, second_last] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: Zone::Hand, to: Zone::Graveyard, .. } if *object_id == id)).count(), 1);
    }
    unchanged_hand(
        &runner,
        &[
            first_hand.first,
            first_hand.creature,
            first_hand.legendary,
            first_hand.land,
            first_hand.foreign,
            second_first,
            second_land,
            second_creature,
        ],
    );
    assert_eq!(runner.state().objects[&large].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&own].zone, Zone::Battlefield);
    terminal(&runner);
    for _ in 0..3 {
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.advance_to_upkeep();
        runner.auto_advance_to_main_phase();
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            break;
        }
    }
    let WaitingFor::TriggerTargetSelection {
        source_id, player, ..
    } = &runner.state().waiting_for
    else {
        panic!("remaining Saga's actual III");
    };
    assert_eq!(*source_id, Some(sagas[1]));
    assert_eq!(*player, P0);
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(PlayerId(2))),
        })
        .unwrap();
    settle(&mut runner, &mut events);
    assert_eq!(runner.state().objects[&second_victim].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&second_last].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&first_victim].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&first_hand.second].zone,
        Zone::Graveyard
    );
    assert_eq!(runner.state().objects[&sagas[0]].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&sagas[1]].zone, Zone::Graveyard);
    terminal(&runner);
}
